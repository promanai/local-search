use localsearch_substring_spike::{BenchmarkOptions, ExperimentError, Strategy, run_benchmark};
use std::env;
use std::path::PathBuf;

fn main() -> Result<(), ExperimentError> {
    let mut options = BenchmarkOptions {
        run_id: String::new(),
        records: 100_000,
        seed: 20_260_814,
        samples_per_cell: 30,
        writer_heap_bytes: 128 * 1_024 * 1_024,
        candidate_limit: 300,
        output_directory: PathBuf::from("reports/spikes/start-003"),
        strategies: Vec::new(),
        memory_bytes: 0,
        storage: String::new(),
        power: String::new(),
    };
    let arguments: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or(ExperimentError::InvalidDocument("missing CLI value"))?;
        match flag.as_str() {
            "--run-id" => options.run_id.clone_from(value),
            "--records" => options.records = parse(value)?,
            "--seed" => options.seed = parse(value)?,
            "--samples" => options.samples_per_cell = parse(value)?,
            "--writer-heap-bytes" => options.writer_heap_bytes = parse(value)?,
            "--candidate-limit" => options.candidate_limit = parse(value)?,
            "--output" => options.output_directory = PathBuf::from(value),
            "--strategy" => options.strategies.push(parse_strategy(value)?),
            "--memory-bytes" => options.memory_bytes = parse(value)?,
            "--storage" => options.storage.clone_from(value),
            "--power" => options.power.clone_from(value),
            _ => return Err(ExperimentError::InvalidDocument("unknown CLI flag")),
        }
        index += 2;
    }

    for report in run_benchmark(&options)? {
        println!("{}", report.display());
    }
    Ok(())
}

fn parse<T>(value: &str) -> Result<T, ExperimentError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| ExperimentError::InvalidDocument("invalid CLI number"))
}

fn parse_strategy(value: &str) -> Result<Strategy, ExperimentError> {
    match value {
        "trigram" => Ok(Strategy::Trigram),
        "token_prefix_limited_trigram" => Ok(Strategy::TokenPrefixLimitedTrigram),
        "bounded_fourgram" => Ok(Strategy::BoundedFourgram),
        _ => Err(ExperimentError::InvalidDocument("unknown strategy")),
    }
}
