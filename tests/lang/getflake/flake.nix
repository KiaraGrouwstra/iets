{ outputs = { self }: { x = 42; me = self.outPath; sub = import ./sub.nix; }; }
