let copy = builtins.path { path = ./data; }; in builtins.path { path = copy; name = "again"; }
