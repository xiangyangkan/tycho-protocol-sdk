//! Utilities to handle relative balances.
//!
//!
//! To aggregate relative balances changes to absolute balances the general approach is:
//!
//! 1. Use a map function that will extract a `BlockBalanceDeltas` message. BalanceDeltas within
//!    this message are required to have increasing ordinals so that the order of relative balance
//!    changes is unambiguous.
//! 2. Store the balances changes with a store handler. You can use the `store_balance_changes`
//!    library method directly for this.
//! 3. In the output module, use aggregate_balance_changes to receive an aggregated map of absolute
//!    balances.
use crate::pb::tycho::evm::v1::{BalanceChange, BlockBalanceDeltas, Transaction};
use itertools::Itertools;
use std::{collections::HashMap, str::FromStr};
use substreams::{
    key,
    pb::substreams::StoreDeltas,
    prelude::{BigInt, StoreAdd},
};

/// Store relative balances changes in a additive manner.
///
/// Effectively aggregates the relative balances changes into an absolute balances.
///
/// ## Arguments
///
/// * `deltas` - A `BlockBalanceDeltas` message containing the relative balances changes. Note:
///   relative balance deltas must have strictly increasing ordinals per token address, will panic
///   otherwise.
/// * `store` - An AddStore that will add relative balance changes.
///
/// This method is meant to be used in combination with `aggregate_balances_changes`
/// which consumes the store filled with this methods in
/// [deltas mode](https://substreams.streamingfast.io/documentation/develop/manifest-modules/types#deltas-mode).
pub fn store_balance_changes(deltas: BlockBalanceDeltas, store: impl StoreAdd<BigInt>) {
    let mut previous_ordinal = HashMap::<String, u64>::new();
    deltas
        .balance_deltas
        .iter()
        .for_each(|delta| {
            let balance_key = format!(
                "{0}:{1}",
                String::from_utf8(delta.component_id.clone())
                    .expect("delta.component_id is not valid utf-8!"),
                hex::encode(&delta.token)
            );
            let current_ord = delta.ord;
            previous_ordinal
                .entry(balance_key.clone())
                .and_modify(|ord| {
                    // ordinals must arrive in increasing order
                    if *ord >= current_ord {
                        panic!(
                            "Invalid ordinal sequence for {}: {} >= {}",
                            balance_key, *ord, current_ord
                        );
                    }
                    *ord = current_ord;
                })
                .or_insert(delta.ord);
            store.add(delta.ord, balance_key, BigInt::from_signed_bytes_be(&delta.delta));
        });
}

type TxAggregatedBalances = HashMap<Vec<u8>, (Transaction, HashMap<Vec<u8>, BalanceChange>)>;

/// Aggregates absolute balances per transaction and token.
///
/// ## Arguments
/// * `balance_store` - A `StoreDeltas` with all changes that occured in the source store module.
/// * `deltas` - A `BlockBalanceDeltas` message containing the relative balances changes.
///
/// Reads absolute balance values from the additive store (see `store_balance_changes`
/// on how to create such a store), proceeds to zip them with the relative balance
/// deltas to associate balance values to token and component.
///
/// Will keep the last balance change per token per transaction if there are multiple
/// changes.
///
/// Returns a map of transactions hashes to the full transaction and aggregated
/// absolute balance changes.
pub fn aggregate_balances_changes(
    balance_store: StoreDeltas,
    deltas: BlockBalanceDeltas,
) -> TxAggregatedBalances {
    balance_store
        .deltas
        .into_iter()
        .zip(deltas.balance_deltas)
        .map(|(store_delta, balance_delta)| {
            let component_id = key::segment_at(&store_delta.key, 0);
            let token_id = key::segment_at(&store_delta.key, 1);
            // store_delta.new_value is an ASCII string representing an integer
            let ascii_string =
                String::from_utf8(store_delta.new_value.clone()).expect("Invalid UTF-8 sequence");
            let balance = BigInt::from_str(&ascii_string).expect("Failed to parse integer");
            let big_endian_bytes_balance = balance.to_bytes_be().1;

            (
                balance_delta
                    .tx
                    .expect("Missing transaction on delta"),
                BalanceChange {
                    token: hex::decode(token_id).expect("Token ID not valid hex"),
                    balance: big_endian_bytes_balance,
                    component_id: component_id.as_bytes().to_vec(),
                },
            )
        })
        // We need to group the balance changes by tx hash for the `TransactionContractChanges` agg
        .group_by(|(tx, _)| tx.hash.clone())
        .into_iter()
        .map(|(txh, group)| {
            let (mut transactions, balance_changes): (Vec<_>, Vec<_>) = group.into_iter().unzip();

            let balances = balance_changes
                .into_iter()
                .map(|balance_change| (balance_change.token.clone(), balance_change))
                .collect();
            (txh, (transactions.pop().unwrap(), balances))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        mock_store::MockStore,
        pb::tycho::evm::v1::{BalanceDelta, Transaction},
    };
    use substreams::{
        pb::substreams::StoreDelta,
        prelude::{StoreGet, StoreNew},
    };

    fn block_balance_deltas() -> BlockBalanceDeltas {
        let comp_id = "0x42c0ffee"
            .to_string()
            .as_bytes()
            .to_vec();
        let token_0 = hex::decode("bad999").unwrap();
        let token_1 = hex::decode("babe00").unwrap();
        BlockBalanceDeltas {
            balance_deltas: vec![
                BalanceDelta {
                    ord: 0,
                    tx: Some(Transaction {
                        hash: vec![0, 1],
                        from: vec![9, 9],
                        to: vec![8, 8],
                        index: 0,
                    }),
                    token: token_0.clone(),
                    delta: BigInt::from_str("+1000")
                        .unwrap()
                        .to_signed_bytes_be(),
                    component_id: comp_id.clone(),
                },
                BalanceDelta {
                    ord: 2,
                    tx: Some(Transaction {
                        hash: vec![0, 1],
                        from: vec![9, 9],
                        to: vec![8, 8],
                        index: 0,
                    }),
                    token: token_1.clone(),
                    delta: BigInt::from_str("+100")
                        .unwrap()
                        .to_signed_bytes_be(),
                    component_id: comp_id.clone(),
                },
                BalanceDelta {
                    ord: 3,
                    tx: Some(Transaction {
                        hash: vec![0, 1],
                        from: vec![9, 9],
                        to: vec![8, 8],
                        index: 0,
                    }),
                    token: token_1.clone(),
                    delta: BigInt::from_str("50")
                        .unwrap()
                        .to_signed_bytes_be(),
                    component_id: comp_id.clone(),
                },
                BalanceDelta {
                    ord: 10,
                    tx: Some(Transaction {
                        hash: vec![0, 1],
                        from: vec![9, 9],
                        to: vec![8, 8],
                        index: 0,
                    }),
                    token: token_0.clone(),
                    delta: BigInt::from_str("-1")
                        .unwrap()
                        .to_signed_bytes_be(),
                    component_id: comp_id.clone(),
                },
            ],
        }
    }
    fn store_deltas() -> StoreDeltas {
        let comp_id = "0x42c0ffee"
            .to_string()
            .as_bytes()
            .to_vec();
        let token_0 = hex::decode("bad999").unwrap();
        let token_1 = hex::decode("babe00").unwrap();

        let t0_key =
            format!("{}:{}", String::from_utf8(comp_id.clone()).unwrap(), hex::encode(token_0));
        let t1_key =
            format!("{}:{}", String::from_utf8(comp_id.clone()).unwrap(), hex::encode(token_1));
        StoreDeltas {
            deltas: vec![
                StoreDelta {
                    operation: 0,
                    ordinal: 0,
                    key: t0_key.clone(),
                    old_value: BigInt::from(0)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                    new_value: BigInt::from(1000)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                },
                StoreDelta {
                    operation: 0,
                    ordinal: 2,
                    key: t1_key.clone(),
                    old_value: BigInt::from(0)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                    new_value: BigInt::from(100)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                },
                StoreDelta {
                    operation: 0,
                    ordinal: 3,
                    key: t1_key.clone(),
                    old_value: BigInt::from(100)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                    new_value: BigInt::from(150)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                },
                StoreDelta {
                    operation: 0,
                    ordinal: 10,
                    key: t0_key.clone(),
                    old_value: BigInt::from(1000)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                    new_value: BigInt::from(999)
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                },
            ],
        }
    }

    #[test]
    fn test_store_balances() {
        let comp_id = "0x42c0ffee"
            .to_string()
            .as_bytes()
            .to_vec();
        let token_0 = hex::decode("bad999").unwrap();
        let token_1 = hex::decode("babe00").unwrap();
        let deltas = block_balance_deltas();
        let store = <MockStore as StoreNew>::new();

        store_balance_changes(deltas, store.clone());
        let res_0 = store.get_last(format!(
            "{}:{}",
            String::from_utf8(comp_id.clone()).unwrap(),
            hex::encode(token_0)
        ));
        let res_1 = store.get_last(format!(
            "{}:{}",
            String::from_utf8(comp_id.clone()).unwrap(),
            hex::encode(token_1)
        ));

        assert_eq!(res_0, Some(BigInt::from_str("+999").unwrap()));
        assert_eq!(res_1, Some(BigInt::from_str("+150").unwrap()));
    }

    #[test]
    fn test_aggregate_balances_changes() {
        let store_deltas = store_deltas();
        let balance_deltas = block_balance_deltas();
        let comp_id = "0x42c0ffee"
            .to_string()
            .as_bytes()
            .to_vec();
        let token_0 = hex::decode("bad999").unwrap();
        let token_1 = hex::decode("babe00").unwrap();

        let exp = [(
            vec![0, 1],
            (
                Transaction { hash: vec![0, 1], from: vec![9, 9], to: vec![8, 8], index: 0 },
                [
                    (
                        token_0.clone(),
                        BalanceChange {
                            token: token_0,
                            balance: BigInt::from(999)
                                .to_signed_bytes_be()
                                .to_vec(),
                            component_id: comp_id.clone(),
                        },
                    ),
                    (
                        token_1.clone(),
                        BalanceChange {
                            token: token_1,
                            balance: vec![150],
                            component_id: comp_id.clone(),
                        },
                    ),
                ]
                .into_iter()
                .collect::<HashMap<_, _>>(),
            ),
        )]
        .into_iter()
        .collect::<HashMap<_, _>>();

        let res = aggregate_balances_changes(store_deltas, balance_deltas);
        assert_eq!(res, exp);
    }
}
