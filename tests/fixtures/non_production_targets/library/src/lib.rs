pub fn product_api() {}

pub enum BenchMode {
    OnlyBench,
}

pub fn bench_api() -> BenchMode {
    BenchMode::OnlyBench
}

pub fn unused() {}
