with builtins;
[
  (fromTOML ''
    title = "TOML Example"
    [owner]
    name = "Tom Preston-Werner"
    [database]
    server = "192.168.1.1"
    ports = [ 8001, 8001, 8002 ]
    connection_max = 5000
    enabled = true
    [servers]
      [servers.alpha]
      ip = "10.0.0.1"
      dc = "eqdc10"
    [clients]
    data = [ ["gamma", "delta"], [1, 2] ]
    hosts = [
      "alpha",
      "omega"
    ]
  '')
  (fromTOML ''
    1234 = "value"
    "127.0.0.1" = "value"
    "character encoding" = "value"
    "ʎǝʞ" = "value"
    'quoted "value"' = "value"
    physical.color = "orange"
    site."google.com" = true
    str = "I'm a string. \"You can quote me\". Name\tJosé\nLocation\tSF."
    int1 = +99
    int5 = 1_000
    hex1 = 0xDEADBEEF
    oct1 = 0o01234567
    bin1 = 0b11010110
    flt1 = +1.0
    flt4 = 5e+22
    flt6 = -2E-2
    flt8 = 9_224_617.445_991_228_313
    x = inf
    y = -inf
    z = nan
    big = 9223372036854775807
    small = -9223372036854775808
    arr4 = [ "all", 'strings', """are the same""", ''''type'''']
    arr8 = [
      1,
      2, # this is ok
    ]
    [dog."tater.man"]
    type.name = "pug"
    [ j . "ʞ" . 'l' ]
    [[products]]
    name = "Hammer"
    [[products]]
    [[products]]
    name = "Nail"
    [[fruit]]
      name = "apple"
      [fruit.physical]
        color = "red"
      [[fruit.variety]]
        name = "red delicious"
    [[fruit]]
      name = "banana"
  '')
  (fromTOML "")
  (fromTOML "a = [ [ { b = true } ] ]\nc = [ [ [ 123 ] ] ]")
]
