[ "${./data}" "${builtins.path { path = ./data; filter = _: _: true; }}" ]
