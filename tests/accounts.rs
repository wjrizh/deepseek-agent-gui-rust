#[test]
fn accounts_visible() {
    let mgr = deepseek_agent::accounts::AccountManager::load_default().unwrap();
    let names: Vec<String> = mgr.list().iter().map(|a| a.name.clone()).collect();
    println!("accounts: {names:?}");
    println!("default: {:?}", mgr.default_account());
    assert!(!names.is_empty(), "no accounts found");
    for n in &names {
        let cfg = deepseek_agent::Config::for_account(n).unwrap();
        println!(
            "{n}: token_file={:?} exists={}",
            cfg.token_file,
            cfg.token_file.exists()
        );
    }
}
