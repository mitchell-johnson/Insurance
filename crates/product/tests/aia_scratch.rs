use ic_product::{load_product, load_rate_table};
#[test]
fn all_load() {
    let mut errs = vec![];
    for e in std::fs::read_dir("../../products").unwrap() {
        let p = e.unwrap().path();
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        if n.starts_with("aia_") && n.ends_with(".star") {
            if let Err(err) = load_product(&n, &std::fs::read_to_string(&p).unwrap()) { errs.push(format!("{n}: {err}")); }
        }
    }
    for e in std::fs::read_dir("../../products/rates").unwrap() {
        let p = e.unwrap().path();
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        if n.starts_with("aia_") {
            if let Err(err) = load_rate_table(&n, &std::fs::read_to_string(&p).unwrap()) { errs.push(format!("{n}: {err}")); }
        }
    }
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}
