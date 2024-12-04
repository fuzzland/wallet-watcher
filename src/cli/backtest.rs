use {
    crate::{
        config::WalletWithContext,
        processor::{self, PnlReport},
        utils::{self, new_provider},
    },
    alloy::{primitives::Address, providers::Provider, transports::Transport},
    alloy_chains::Chain,
    clap::Parser,
    eyre::{ensure, eyre, Context, ContextCompat},
    serde::{Deserialize, Serialize},
    std::{fs::File, sync::Arc, time::Duration},
    tokio::{
        sync::{mpsc::unbounded_channel, Semaphore},
        time::Instant,
    },
};

#[derive(Debug, Parser)]
pub struct Args {
    #[arg(help = "Path to test data")]
    test_data: String,

    #[arg(long, env = "ETH_RPC_URL", help = "Ethereum RPC URL")]
    rpc_url: String,

    #[arg(long, help = "Append to existing backtest data")]
    generate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub remark: String,

    // Input
    pub block: u64,
    pub address: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<Address>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_addresses: Vec<Address>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_recipient: bool,

    // Output
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<PnlReport>,
}

impl std::fmt::Display for TestCase {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}", self.address, self.block)
    }
}

pub struct RunResult {
    pub index: usize,
    pub test_case: TestCase,
    pub report: eyre::Result<Option<PnlReport>>,
    pub elapsed: Duration,
}

impl Args {
    pub async fn run(self) {
        let file = File::open(self.test_data.clone()).expect("Failed to open test data file");
        let test_cases: Vec<TestCase> = serde_yaml::from_reader(file).expect("Failed to parse test data");

        let provider = new_provider(&self.rpc_url).await.expect("Failed to create provider");

        let chain = provider.get_chain_id().await.expect("Failed to get chain").into();
        let rpc_url = self.rpc_url.clone();

        let (sender, mut receiver) = unbounded_channel::<RunResult>();

        tokio::spawn(async move {
            let semaphore = Arc::new(Semaphore::new(num_cpus::get() * 2)); // networkIO bounded
            for (i, test_case) in test_cases.into_iter().enumerate() {
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let sender = sender.clone();
                let rpc_url = rpc_url.clone();

                tokio::spawn(async move {
                    let start = Instant::now();
                    let report = match new_provider(&rpc_url).await {
                        Ok(p) => worker(chain, p.as_ref(), &test_case).await,
                        Err(e) => Err(eyre!("Failed to create provider: {e:#}")),
                    };
                    let elapsed = start.elapsed();

                    let result = RunResult {
                        index: i,
                        test_case,
                        report,
                        elapsed,
                    };

                    sender.send(result).unwrap();
                    drop(permit);
                });
            }
        });

        let mut generated_test_case = Vec::new();
        let mut unmatched_cases = Vec::new();
        let mut failed_cases = Vec::new();

        while let Some(result) = receiver.recv().await {
            if let Some(err) = result.report.as_ref().err() {
                println!(
                    "[{}] Elapsed: {:?}, Failed: {:#}",
                    result.test_case, result.elapsed, err
                );
                failed_cases.push(result.test_case.clone());
            }

            if self.generate {
                if result.report.is_ok() {
                    println!("[{}] Elapsed: {:?}", result.test_case, result.elapsed);
                }

                generated_test_case.push((
                    result.index,
                    TestCase {
                        report: result.report.unwrap_or_default(),
                        ..result.test_case
                    },
                ));

                continue;
            }

            let Ok(report) = result.report else {
                continue;
            };

            let passed = report == result.test_case.report;
            if !passed {
                unmatched_cases.push((result.test_case.clone(), report));
            }

            println!(
                "[{}] Elapsed: {:?}, {}",
                result.test_case,
                result.elapsed,
                if passed { "Passed" } else { "Unmatched" }
            );
        }

        if self.generate {
            // Following the original order
            generated_test_case.sort_by_key(|(i, _)| *i);
            let ordered_test_cases: Vec<_> = generated_test_case.into_iter().map(|(_, tc)| tc).collect();

            let file = File::create(self.test_data).expect("Failed to create test data file");
            serde_yaml::to_writer(file, &ordered_test_cases).expect("Failed to write test data");
            return;
        }

        if unmatched_cases.is_empty() {
            println!("All tests passed!");
            return;
        }

        for (i, (test_case, report)) in unmatched_cases.into_iter().enumerate() {
            let mut cmd = format!(
                "RUST_LOG=wallet_watcher=trace cargo run run-block {} {}",
                test_case.block, test_case.address
            );

            if !test_case.other_addresses.is_empty() {
                cmd.push_str(&format!(
                    " -a {}",
                    test_case
                        .other_addresses
                        .iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }

            if let Some(builder) = test_case.builder {
                cmd.push_str(&format!(" -b {}", builder));
            }

            if test_case.include_recipient {
                cmd.push_str(" --include-recipient");
            }

            println!("=== Unmatched Case #{i}: {test_case} ===");
            println!("Debug command: {cmd}");
            println!("Expected: ");
            println!("{:#?}", test_case.report);
            println!("Actual: ");
            println!("{report:#?}");
        }

        println!("=== Failed Case ===");
        for test_case in failed_cases {
            println!("{test_case}");
        }
    }
}

async fn worker<T: Clone + Transport>(
    chain: Chain,
    provider: &dyn Provider<T>,
    test_case: &TestCase,
) -> eyre::Result<Option<PnlReport>> {
    println!("[{test_case}] Running");

    let receipt_and_traces = utils::get_receipt_and_trace(provider, test_case.block)
        .await
        .context("Failed to get receipt and traces")?;

    let block = provider
        .get_block_by_number(test_case.block.into(), false)
        .await
        .context("Failed to get block")?
        .context("Block not found")?;

    let reports = processor::process_block(
        chain,
        &block.header,
        &receipt_and_traces,
        &[WalletWithContext::new(
            "Testcase".to_string(),
            test_case.address,
            test_case.builder,
            test_case.other_addresses.clone(),
            test_case.include_recipient,
            Arc::default(),
        )],
    )
    .context("Failed to generate report")?;

    ensure!(reports.len() == 1, "Expected exactly one report");
    Ok(reports.into_iter().next().unwrap())
}

fn is_false(v: &bool) -> bool {
    !v
}
