use {
    crate::config::NATIVE_TOKEN,
    alloy::primitives::{Address, I256, U256},
    alloy_chains::Chain,
    serde::{Deserialize, Serialize},
    std::{
        collections::HashMap,
        ops::{Deref, DerefMut},
    },
    tracing::trace,
};

/// Account to `token:balance change`
#[derive(Clone, Default)]
pub struct BalanceChanges(HashMap<Address, BalanceChange>);

impl BalanceChanges {
    pub fn append_transfer(&mut self, token: Address, from: Address, to: Address, value: U256) {
        trace!(?token, ?from, ?to, ?value);

        let value = I256::from_raw(value);

        if !from.is_zero() {
            self.entry(from)
                .or_default()
                .entry(token)
                .and_modify(|e| *e -= value)
                .or_insert(-value);
        }

        if !to.is_zero() {
            self.entry(to)
                .or_default()
                .entry(token)
                .and_modify(|e| *e += value)
                .or_insert(value);
        }
    }

    pub fn retain_non_zero(&mut self) {
        self.retain(|_, bc| {
            bc.retain_non_zero();
            !bc.is_empty()
        });
    }
}

impl Deref for BalanceChanges {
    type Target = HashMap<Address, BalanceChange>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BalanceChanges {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::fmt::Debug for BalanceChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            write!(f, "{{}}")?;
            return Ok(());
        }

        const INDENTATION: &str = "    ";
        let pretty = f.alternate();

        if pretty {
            writeln!(f, "{{")?;
        } else {
            write!(f, "{{")?;
        }

        for (i, (account, bc)) in self.0.iter().enumerate() {
            let ending = if i == self.0.len() - 1 { "" } else { "," };

            if pretty {
                writeln!(f, "{INDENTATION}{account:?}: {{")?;
            } else {
                write!(f, "{account:?}:{{")?;
            }

            for (i, (token, value)) in bc.iter().enumerate() {
                let ending = if i == bc.len() - 1 { "" } else { "," };

                if pretty {
                    writeln!(f, "{INDENTATION}{INDENTATION}{token:?}: {value}{ending}")?;
                } else {
                    write!(f, "{token:?}:{value}{ending}")?;
                }
            }

            if pretty {
                writeln!(f, "{INDENTATION}}}{ending}")?;
            } else {
                write!(f, "}}{ending}")?;
            }
        }

        write!(f, "}}")?;

        Ok(())
    }
}

/// Token to balance changes
#[derive(Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct BalanceChange(HashMap<Address, I256>);

impl BalanceChange {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn extend(&mut self, other: &BalanceChange) {
        for (token, change) in other.iter() {
            self.entry(*token).and_modify(|e| *e += *change).or_insert(*change);
        }
    }

    /// Extract ether from the balance change, including WETH
    pub fn extract_ether(&mut self, chain: Chain) -> I256 {
        let weth = chain
            .named()
            .and_then(|n| n.wrapped_native_token())
            .and_then(|weth| self.remove(&weth))
            .unwrap_or(I256::ZERO);

        let eth = self.remove(&NATIVE_TOKEN).unwrap_or(I256::ZERO);

        eth + weth
    }

    pub fn retain_non_zero(&mut self) {
        self.retain(|_, v| !v.is_zero());
    }
}

impl Deref for BalanceChange {
    type Target = HashMap<Address, I256>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BalanceChange {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::fmt::Debug for BalanceChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            write!(f, "{{}}")?;
            return Ok(());
        }

        const INDENTATION: &str = "    ";
        let pretty = f.alternate();

        if pretty {
            writeln!(f, "{{")?;
        } else {
            write!(f, "{{")?;
        }

        for (i, (token, change)) in self.0.iter().enumerate() {
            let ending = if i == self.0.len() - 1 { "" } else { "," };

            if pretty {
                writeln!(f, "{INDENTATION}{token:?}: {change}{ending}")?;
            } else {
                write!(f, "{token:?}:{change}{ending}")?;
            }
        }

        write!(f, "}}")?;

        Ok(())
    }
}
