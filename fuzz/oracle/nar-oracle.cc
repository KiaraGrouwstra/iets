// Nix 2.34.8's NAR reader and writer as a long-lived process, for the nar
// targets of fuzz/. Frames on stdin and stdout, see fuzz/src/nar.rs.

#include <nix/util/archive.hh>
#include <nix/util/memory-source-accessor.hh>
#include <nix/util/serialise.hh>
#include <nix/util/util.hh>

#include <cstdint>
#include <cstring>
#include <string>
#include <unistd.h>

using namespace nix;

namespace {

bool readAll(void * buf, size_t n)
{
    auto p = (char *) buf;
    while (n) {
        auto r = read(0, p, n);
        if (r <= 0)
            return false;
        p += r;
        n -= r;
    }
    return true;
}

void writeAll(const void * buf, size_t n)
{
    auto p = (const char *) buf;
    while (n) {
        auto r = write(1, p, n);
        if (r <= 0)
            _exit(1);
        p += r;
        n -= r;
    }
}

void reply(uint8_t kind, const std::string & payload)
{
    uint32_t len = payload.size();
    writeAll(&kind, 1);
    writeAll(&len, 4);
    writeAll(payload.data(), len);
}

// op 0: read a NAR. The reply is the bytes it consumed (u64) and the tree
// written out again as a NAR.
std::string parse(std::string_view nar)
{
    MemorySourceAccessor tree;
    MemorySink sink{tree};
    StringSource source{nar};
    parseDump(sink, source);
    StringSink out;
    tree.dumpPath(CanonPath::root, out);
    uint64_t consumed = source.pos;
    return std::string((const char *) &consumed, 8) + out.s;
}

// op 1: the NAR of a path on disk.
std::string dump(const std::string & path)
{
    StringSink out;
    dumpPath(path, out);
    return out.s;
}

} // namespace

int main()
{
    initLibUtil();
    std::string input;
    while (true) {
        uint8_t op;
        uint32_t len;
        if (!readAll(&op, 1) || !readAll(&len, 4))
            return 0;
        input.resize(len);
        if (!readAll(input.data(), len))
            return 0;
        try {
            reply(0, op == 0 ? parse(input) : dump(input));
        } catch (std::exception & e) {
            reply(1, e.what());
        }
    }
}
