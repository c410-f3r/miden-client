use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use miden_objects::{
    Digest,
    account::AccountId,
    block::BlockNumber,
    transaction::{OutputNotes, TransactionScript},
};
use miden_tx::utils::Deserializable;
use serde_wasm_bindgen::from_value;
use wasm_bindgen_futures::JsFuture;

use super::{WebStore, account::utils::update_account, note::utils::apply_note_updates_tx};
use crate::{
    store::{StoreError, TransactionFilter},
    transaction::{TransactionRecord, TransactionStatus, TransactionStoreUpdate},
};

mod js_bindings;
use js_bindings::idxdb_get_transactions;

mod models;
use models::TransactionIdxdbObject;

pub mod utils;
use utils::insert_proven_transaction_data;

impl WebStore {
    pub async fn get_transactions(
        &self,
        filter: TransactionFilter,
    ) -> Result<Vec<TransactionRecord>, StoreError> {
        let filter_as_str = match filter {
            TransactionFilter::All => "All",
            TransactionFilter::Uncomitted => "Uncomitted",
            TransactionFilter::Ids(ids) => &{
                let ids_str =
                    ids.iter().map(ToString::to_string).collect::<Vec<String>>().join(",");
                format!("Ids:{ids_str}")
            },
            TransactionFilter::ExpiredBefore(block_number) => {
                &format!("ExpiredPending:{block_number}")
            },
        };

        let promise = idxdb_get_transactions(filter_as_str.to_string());
        let js_value = JsFuture::from(promise).await.map_err(|js_error| {
            StoreError::DatabaseError(format!("failed to get transactions: {js_error:?}"))
        })?;
        let transactions_idxdb: Vec<TransactionIdxdbObject> = from_value(js_value)
            .map_err(|err| StoreError::DatabaseError(format!("failed to deserialize {err:?}")))?;

        let transaction_records: Result<Vec<TransactionRecord>, StoreError> = transactions_idxdb
            .into_iter()
            .map(|tx_idxdb| {
                let native_account_id = AccountId::from_hex(&tx_idxdb.account_id)?;
                let block_num: BlockNumber = tx_idxdb.block_num.parse::<u32>().unwrap().into();
                let commit_height: Option<BlockNumber> =
                    tx_idxdb.commit_height.map(|height| height.parse::<u32>().unwrap().into());

                let id: Digest = tx_idxdb.id.try_into()?;
                let init_account_state: Digest = tx_idxdb.init_account_state.try_into()?;

                let final_account_state: Digest = tx_idxdb.final_account_state.try_into()?;

                let input_note_nullifiers: Vec<Digest> =
                    Vec::<Digest>::read_from_bytes(&tx_idxdb.input_notes)?;

                let output_notes = OutputNotes::read_from_bytes(&tx_idxdb.output_notes)?;

                let transaction_script: Option<TransactionScript> =
                    if tx_idxdb.script_root.is_some() {
                        let tx_script = tx_idxdb
                            .tx_script
                            .map(|script| TransactionScript::read_from_bytes(&script))
                            .transpose()?
                            .expect("Transaction script should be included in the row");

                        Some(tx_script)
                    } else {
                        None
                    };

                let transaction_status = match (commit_height, tx_idxdb.discarded) {
                    (_, true) => TransactionStatus::Discarded,
                    (Some(block_num), false) => TransactionStatus::Committed(block_num),
                    (None, false) => TransactionStatus::Pending,
                };

                Ok(TransactionRecord {
                    id: id.into(),
                    account_id: native_account_id,
                    init_account_state,
                    final_account_state,
                    input_note_nullifiers,
                    output_notes,
                    transaction_script,
                    block_num,
                    transaction_status,
                })
            })
            .collect();

        transaction_records
    }

    pub async fn apply_transaction(
        &self,
        tx_update: TransactionStoreUpdate,
    ) -> Result<(), StoreError> {
        // Transaction Data
        insert_proven_transaction_data(tx_update.executed_transaction()).await?;

        // Account Data
        update_account(tx_update.updated_account()).await.map_err(|err| {
            StoreError::DatabaseError(format!("failed to update account: {err:?}"))
        })?;

        // Updates for notes
        apply_note_updates_tx(tx_update.note_updates()).await?;

        for tag_record in tx_update.new_tags() {
            self.add_note_tag(*tag_record).await?;
        }

        Ok(())
    }
}
