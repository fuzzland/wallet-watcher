use {
    alloy::primitives::Address,
    clap::Parser,
    eyre::{ensure, Context},
    serde::{Deserialize, Serialize},
    std::{collections::HashMap, sync::Arc},
};

pub const NATIVE_TOKEN: Address = Address::ZERO;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Config {
    pub chains: HashMap<String, String>,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Channel {
    #[serde(flatten)]
    pub alert: AlertTo,
    pub wallets: Vec<Wallet>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AlertTo {
    pub bot_token: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Parser)]
#[serde(rename_all = "snake_case")]
pub struct Wallet {
    /// The name of the wallet
    pub name: String,

    /// The address of the wallet
    pub address: Address,

    /// The builder address of the wallet
    pub builder: Option<Address>,

    /// Additional addresses to include in the PnL calculations.
    #[serde(default = "Vec::new")]
    pub other_addresses: Vec<Address>,

    /// Chains this wallet is listening on. Leave empty to listen on all chains.
    #[serde(default = "Vec::new")]
    pub chains: Vec<String>,

    /// If true, the recipient will be included in PnL calculations
    #[serde(default = "Default::default")]
    pub include_recipient: bool,
}

impl Config {
    pub fn from_file(path: &str) -> eyre::Result<Self> {
        let file = std::fs::File::open(path).context("Failed to open config file")?;
        let reader = std::io::BufReader::new(file);
        let config: Config = serde_yaml::from_reader(reader).context("Failed to parse config")?;

        config.validate().context("Invalid config")?;

        Ok(config)
    }

    /// Validate
    ///   1. Chain exists for wallet
    ///   2. Each channel has at least one wallet
    pub fn validate(&self) -> eyre::Result<()> {
        for (i, channel) in self.channels.iter().enumerate() {
            ensure!(!channel.wallets.is_empty(), "Channel #{i} has no wallets",);

            for wallet in &channel.wallets {
                for chain in &wallet.chains {
                    ensure!(
                        self.chains.contains_key(chain),
                        "Chain {} not found for wallet {}",
                        chain,
                        wallet.name
                    );
                }
            }
        }

        Ok(())
    }

    pub fn to_wallet_with_context_by_chain(&self) -> HashMap<String, Vec<WalletWithContext>> {
        let mut result: HashMap<String, Vec<WalletWithContext>> = HashMap::new();

        let all_chains = self.chains.keys().cloned().collect::<Vec<_>>();

        for channel in &self.channels {
            let alert = Arc::new(channel.alert.clone());

            for wallet in &channel.wallets {
                let supported_chains = if wallet.chains.is_empty() {
                    all_chains.iter()
                } else {
                    wallet.chains.iter()
                };

                let wallet = WalletWithContext::new(
                    wallet.name.clone(),
                    wallet.address,
                    wallet.builder,
                    wallet.other_addresses.clone(),
                    wallet.include_recipient,
                    Arc::clone(&alert),
                );

                for chain in supported_chains {
                    result.entry(chain.to_owned()).or_default().push(wallet.clone());
                }
            }
        }

        result
    }
}

#[derive(Clone)]
pub struct WalletWithContext {
    pub name: String,
    pub address: Address,
    pub builder: Option<Address>,
    pub include_recipient: bool,
    pub alert_to: Arc<AlertTo>,

    involved_wallets: Vec<Address>,
}

impl WalletWithContext {
    pub fn new(
        name: String,
        address: Address,
        builder: Option<Address>,
        other_addresses: Vec<Address>,
        include_recipient: bool,
        alert_to: Arc<AlertTo>,
    ) -> Self {
        let involved_wallets = std::slice::from_ref(&address)
            .iter()
            .chain(builder.iter())
            .chain(other_addresses.iter())
            .cloned()
            .collect();

        Self {
            name,
            address,
            builder,
            include_recipient,
            alert_to,
            involved_wallets,
        }
    }

    pub fn involved_wallets(&self) -> &[Address] {
        &self.involved_wallets
    }
}
