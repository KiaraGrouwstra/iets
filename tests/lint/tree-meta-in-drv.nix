let self = builtins.getFlake (toString ./tree-meta-in-drv.d); in
(derivation { name = "x"; system = "s"; builder = "b"; version = "1.0-${builtins.substring 0 8 self.lastModifiedDate}"; }).drvPath
