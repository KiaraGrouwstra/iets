//! A store directory set by `NIX_STORE_DIR` reaches the evaluator, as in Nix.

mod hermetic;

#[test]
fn store_dir_follows_nix_store_dir() {
    let expr = r#"[ builtins.storeDir (derivation { name = "x"; system = "x"; builder = "/b"; }).drvPath ]"#;
    let o = hermetic::iets()
        .env("NIX_STORE_DIR", "/tmp/other/store")
        .args(["eval", "--json", "--strict", "--expr", expr])
        .output()
        .expect("run iets");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Vec<String> = serde_json::from_str(&out).expect("json list");
    assert_eq!(v[0], "/tmp/other/store");
    assert!(v[1].starts_with("/tmp/other/store/"), "{out}");
}
