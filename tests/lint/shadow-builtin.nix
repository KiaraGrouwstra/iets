let map = 1; in { a = map; b = ({ toString }: toString) { toString = 2; }; }
