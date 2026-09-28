use deepseek_agent::client::pow::PowSolver;

#[test]
fn test_embedded_wasm_loads_and_solves() {
    let bytes = include_bytes!("../wasm/sha3.wasm");
    let mut s = PowSolver::new(bytes).expect("load embedded wasm");
    let ans = s
        .solve_challenge(
            "430f64a069c8b8204f7fd1fd79d5b10584b26fc55615616e3170b9f627d7eaf8",
            "b05cab0e3998e06ca822",
            1789025147648,
            144000,
        )
        .expect("solve");
    assert_eq!(ans, 22195.0);
}
