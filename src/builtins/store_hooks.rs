//! Evaluator-side use of the pure `store` module: copying source paths to
//! the (virtual) store and search-path lookup.

use std::collections::BTreeSet;

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::store::path::{IngestionMethod, make_fixed_output_path};

/// Store path a source path would get when copied (`"${./foo}"`). Nothing
/// is written; the NAR hash is what `fetchToStore` would compute.
pub fn copy_path_to_store(
    ev: &mut Evaluator,
    path: &'static [u8],
    pos: Pos,
) -> EvalResult<&'static [u8]> {
    if let Some(sp) = ev.src_to_store.get(path) {
        return Ok(sp);
    }
    let p = std::path::Path::new(
        std::str::from_utf8(path)
            .map_err(|_| Error::boxed(ErrorKind::Eval, "path is not valid UTF-8", pos))?,
    );
    let hash = crate::store::nar::nar_hash(p).map_err(|e| {
        Error::boxed(
            ErrorKind::Io,
            format!(
                "copying '{}' to the store: {}",
                String::from_utf8_lossy(path),
                e
            ),
            pos,
        )
    })?;
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".into());
    let name = if name.is_empty() {
        "source".to_string()
    } else {
        name
    };
    let sp = make_fixed_output_path(IngestionMethod::Recursive, &hash, &name, &BTreeSet::new())
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    ev.src_to_store.insert(path.to_vec(), full);
    ev.sources.insert(
        sp,
        crate::store::source::SourceOrigin::Tree {
            local: p.to_path_buf(),
            accepted: None,
        },
    );
    Ok(full)
}
