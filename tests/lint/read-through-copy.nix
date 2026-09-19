let copy = "${./data}"; in builtins.readFile "${copy}/hello.txt"
