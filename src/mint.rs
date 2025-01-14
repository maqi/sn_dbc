// Copyright 2021 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

// Code required to mint Dbcs
// The in the most basic terms means
// a valid input DBC can be split into
// 1 or more DBCs as long as
// input is vaid
// Outputs <= input value

use crate::{
    Dbc, DbcContent, DbcContentHash, DbcTransaction, Error, Hash, KeyManager, NodeSignature,
    PublicKeySet, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    iter::FromIterator,
};

pub type MintSignatures = BTreeMap<DbcContentHash, (PublicKeySet, NodeSignature)>;

pub const GENESIS_DBC_INPUT: Hash = Hash([0u8; 32]);

pub trait SpendBook: std::fmt::Debug + Clone {
    type Error: std::error::Error;

    fn lookup(&self, dbc_hash: &DbcContentHash) -> Result<Option<&DbcTransaction>, Self::Error>;
    fn log(
        &mut self,
        dbc_hash: DbcContentHash,
        transaction: DbcTransaction,
    ) -> Result<(), Self::Error>;

    fn entries(&self) -> Box<dyn Iterator<Item = (&DbcContentHash, &DbcTransaction)> + '_>;
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SimpleSpendBook {
    pub transactions: BTreeMap<DbcContentHash, DbcTransaction>,
}

impl SpendBook for SimpleSpendBook {
    type Error = std::convert::Infallible;

    fn lookup(&self, dbc_hash: &DbcContentHash) -> Result<Option<&DbcTransaction>, Self::Error> {
        Ok(self.transactions.get(dbc_hash))
    }

    fn log(
        &mut self,
        dbc_hash: DbcContentHash,
        transaction: DbcTransaction,
    ) -> Result<(), Self::Error> {
        self.transactions.insert(dbc_hash, transaction);
        Ok(())
    }

    fn entries(&self) -> Box<dyn Iterator<Item = (&DbcContentHash, &DbcTransaction)> + '_> {
        Box::new(self.transactions.iter())
    }
}

impl SimpleSpendBook {
    pub fn new() -> Self {
        Self {
            transactions: Default::default(),
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone, Deserialize, Serialize)]
pub struct ReissueTransaction {
    pub inputs: HashSet<Dbc>,
    pub outputs: HashSet<DbcContent>,
}

impl ReissueTransaction {
    pub fn blinded(&self) -> DbcTransaction {
        DbcTransaction {
            inputs: BTreeSet::from_iter(self.inputs.iter().map(|i| i.name())),
            outputs: BTreeSet::from_iter(self.outputs.iter().map(|i| i.hash())),
        }
    }

    pub fn validate<K: KeyManager>(&self, verifier: &K) -> Result<()> {
        self.validate_balance()?;
        self.validate_input_dbcs(verifier)?;
        self.validate_outputs()?;
        Ok(())
    }

    fn validate_balance(&self) -> Result<()> {
        let input: u64 = self.inputs.iter().map(|input| input.amount()).sum();
        let output: u64 = self.outputs.iter().map(|output| output.amount).sum();

        if input != output {
            Err(Error::DbcReissueRequestDoesNotBalance { input, output })
        } else {
            Ok(())
        }
    }

    fn validate_input_dbcs<K: KeyManager>(&self, verifier: &K) -> Result<()> {
        if self.inputs.is_empty() {
            return Err(Error::TransactionMustHaveAnInput);
        }

        for input in self.inputs.iter() {
            input.confirm_valid(verifier)?;
        }

        Ok(())
    }

    fn validate_outputs(&self) -> Result<()> {
        // Validate outputs are numbered 0..N_OUTPUTS
        let number_set = BTreeSet::from_iter(
            self.outputs
                .iter()
                .map(|dbc_content| dbc_content.output_number),
        );

        let expected_number_set = BTreeSet::from_iter(0..self.outputs.len() as u32);

        if number_set != expected_number_set {
            println!(
                "output numbering is wrong {:?} != {:?}",
                number_set, expected_number_set
            );
            return Err(Error::OutputsAreNotNumberedCorrectly);
        }

        // Validate output parents match the blinded inputs
        let inputs = self.blinded().inputs;
        if self.outputs.iter().any(|o| o.parents != inputs) {
            return Err(Error::DbcContentParentsDifferentFromTransactionInputs);
        }

        Ok(())
    }
}

#[derive(Eq, PartialEq, Debug, Clone, Deserialize, Serialize)]
pub struct ReissueRequest {
    pub transaction: ReissueTransaction,
    // Signatures from the owners of each input, signing `self.transaction.blinded().hash()`
    pub input_ownership_proofs: HashMap<DbcContentHash, (blsttc::PublicKey, blsttc::Signature)>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mint<K, S>
where
    K: KeyManager,
    S: SpendBook,
{
    pub(crate) key_manager: K,
    pub spendbook: S,
}

impl<K: KeyManager, S: SpendBook> Mint<K, S> {
    pub fn new(key_manager: K, spendbook: S) -> Self {
        Self {
            key_manager,
            spendbook,
        }
    }

    pub fn issue_genesis_dbc(
        &mut self,
        amount: u64,
    ) -> Result<(DbcContent, DbcTransaction, (PublicKeySet, NodeSignature))> {
        let parents = BTreeSet::from_iter(vec![GENESIS_DBC_INPUT]);
        let content = DbcContent::new(
            parents,
            amount,
            0,
            self.key_manager
                .public_key_set()
                .map_err(|e| Error::Signing(e.to_string()))?
                .public_key(),
        );
        let transaction = DbcTransaction {
            inputs: BTreeSet::from_iter(vec![GENESIS_DBC_INPUT]),
            outputs: BTreeSet::from_iter(vec![content.hash()]),
        };

        match self
            .spendbook
            .lookup(&GENESIS_DBC_INPUT)
            .map_err(|e| Error::SpendBook(e.to_string()))?
        {
            Some(tx) if tx != &transaction => return Err(Error::GenesisInputAlreadySpent),
            _ => (),
        }

        self.spendbook
            .log(GENESIS_DBC_INPUT, transaction.clone())
            .map_err(|e| Error::SpendBook(e.to_string()))?;
        let transaction_sig = self
            .key_manager
            .sign(&transaction.hash())
            .map_err(|e| Error::Signing(e.to_string()))?;

        Ok((
            content,
            transaction,
            (
                self.key_manager
                    .public_key_set()
                    .map_err(|e| Error::Signing(e.to_string()))?,
                transaction_sig,
            ),
        ))
    }

    pub fn is_spent(&self, dbc_hash: DbcContentHash) -> Result<bool> {
        Ok(self
            .spendbook
            .lookup(&dbc_hash)
            .map_err(|e| Error::SpendBook(e.to_string()))?
            .is_some())
    }

    pub fn key_manager(&self) -> &K {
        &self.key_manager
    }

    pub fn reissue(
        &mut self,
        reissue_req: ReissueRequest,
        inputs_belonging_to_mint: BTreeSet<DbcContentHash>,
    ) -> Result<(DbcTransaction, MintSignatures)> {
        reissue_req.transaction.validate(self.key_manager())?;
        let transaction = reissue_req.transaction.blinded();
        let transaction_hash = transaction.hash();

        for input_dbc in reissue_req.transaction.inputs.iter() {
            match reissue_req.input_ownership_proofs.get(&input_dbc.name()) {
                Some((owner, sig)) if owner.verify(&sig, &transaction_hash) => {
                    input_dbc.content.validate_unblinding(owner)?;
                }
                Some(_) => return Err(Error::FailedSignature),
                None => return Err(Error::MissingInputOwnerProof),
            }
        }

        if !inputs_belonging_to_mint.is_subset(&transaction.inputs) {
            return Err(Error::FilteredInputNotPresent);
        }

        // Validate that each input has not yet been spent.
        for input in inputs_belonging_to_mint.iter() {
            if let Some(transaction) = self
                .spendbook
                .lookup(&input)
                .map_err(|e| Error::SpendBook(e.to_string()))?
                .cloned()
            {
                // This input has already been spent, return the spend transaction to the user
                let transaction_sigs = self.sign_transaction(&transaction)?;
                return Err(Error::DbcAlreadySpent {
                    transaction,
                    transaction_sigs,
                });
            }
        }

        let transaction_sigs = self.sign_transaction(&transaction)?;

        for input in reissue_req
            .transaction
            .inputs
            .iter()
            .filter(|&i| inputs_belonging_to_mint.contains(&i.name()))
        {
            self.spendbook
                .log(input.name(), transaction.clone())
                .map_err(|e| Error::SpendBook(e.to_string()))?;
        }

        Ok((transaction, transaction_sigs))
    }

    fn sign_transaction(
        &self,
        transaction: &DbcTransaction,
    ) -> Result<BTreeMap<DbcContentHash, (PublicKeySet, NodeSignature)>> {
        let sig = self
            .key_manager
            .sign(&transaction.hash())
            .map_err(|e| Error::Signing(e.to_string()))?;

        Ok(transaction
            .inputs
            .iter()
            .copied()
            .zip(std::iter::repeat((
                self.key_manager
                    .public_key_set()
                    .map_err(|e| Error::Signing(e.to_string()))?,
                sig,
            )))
            .collect())
    }

    // Used in testing / benchmarking
    pub fn snapshot_spendbook(&self) -> S {
        self.spendbook.clone()
    }

    // Used in testing / benchmarking
    pub fn reset_spendbook(&mut self, spendbook: S) {
        self.spendbook = spendbook
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    use crate::{
        tests::{TinyInt, TinyVec},
        SimpleKeyManager, SimpleSigner,
    };

    #[quickcheck]
    fn prop_genesis() {
        let genesis_owner = crate::bls_dkg_id();
        let genesis_key = genesis_owner.public_key_set.public_key();

        let key_manager = SimpleKeyManager::new(
            SimpleSigner::new(
                genesis_owner.public_key_set.clone(),
                (0, genesis_owner.secret_key_share.clone()),
            ),
            genesis_owner.public_key_set.public_key(),
        );
        let mut genesis_node = Mint::new(key_manager, SimpleSpendBook::new());

        let (gen_dbc_content, gen_dbc_trans, (gen_key_set, gen_node_sig)) =
            genesis_node.issue_genesis_dbc(1000).unwrap();

        let genesis_sig = gen_key_set
            .combine_signatures(vec![gen_node_sig.threshold_crypto()])
            .unwrap();

        let genesis_dbc = Dbc {
            content: gen_dbc_content,
            transaction: gen_dbc_trans,
            transaction_sigs: BTreeMap::from_iter(vec![(
                GENESIS_DBC_INPUT,
                (genesis_key, genesis_sig),
            )]),
        };

        assert_eq!(genesis_dbc.content.amount, 1000);
        let validation = genesis_dbc.confirm_valid(genesis_node.key_manager());
        assert!(validation.is_ok());
    }

    #[quickcheck]
    fn prop_splitting_the_genesis_dbc(output_amounts: TinyVec<TinyInt>) {
        let output_amounts = Vec::from_iter(output_amounts.into_iter().map(TinyInt::coerce::<u64>));
        let output_amount = output_amounts.iter().sum();

        let genesis_owner = crate::bls_dkg_id();
        let genesis_key = genesis_owner.public_key_set.public_key();
        let key_manager = SimpleKeyManager::new(
            SimpleSigner::new(
                genesis_owner.public_key_set.clone(),
                (0, genesis_owner.secret_key_share.clone()),
            ),
            genesis_owner.public_key_set.public_key(),
        );
        let mut genesis_node = Mint::new(key_manager.clone(), SimpleSpendBook::new());

        let (gen_dbc_content, gen_dbc_trans, (gen_key_set, gen_node_sig)) =
            genesis_node.issue_genesis_dbc(output_amount).unwrap();
        let genesis_sig = gen_key_set
            .combine_signatures(vec![gen_node_sig.threshold_crypto()])
            .unwrap();

        let genesis_dbc = Dbc {
            content: gen_dbc_content,
            transaction: gen_dbc_trans,
            transaction_sigs: BTreeMap::from_iter(vec![(
                GENESIS_DBC_INPUT,
                (genesis_key, genesis_sig),
            )]),
        };

        let inputs = HashSet::from_iter(vec![genesis_dbc.clone()]);
        let input_hashes = BTreeSet::from_iter(inputs.iter().map(|in_dbc| in_dbc.name()));

        let output_owner = crate::bls_dkg_id();
        let outputs = HashSet::from_iter(output_amounts.iter().enumerate().map(|(i, amount)| {
            DbcContent::new(
                input_hashes.clone(),
                *amount,
                i as u32,
                output_owner.public_key_set.public_key(),
            )
        }));

        let transaction = ReissueTransaction { inputs, outputs };

        let sig_share = genesis_owner
            .secret_key_share
            .sign(&transaction.blinded().hash());

        let sig = genesis_owner
            .public_key_set
            .combine_signatures(vec![(0, &sig_share)])
            .unwrap();

        let reissue_req = ReissueRequest {
            transaction,
            input_ownership_proofs: HashMap::from_iter(vec![(
                genesis_dbc.name(),
                (genesis_owner.public_key_set.public_key(), sig),
            )]),
        };

        let (transaction, transaction_sigs) = genesis_node
            .reissue(reissue_req.clone(), input_hashes)
            .unwrap();

        // Verify transaction returned to us by the Mint matches our request
        assert_eq!(reissue_req.transaction.blinded(), transaction);

        // Verify signatures corespond to each input
        let (pub_key_set, sig) = transaction_sigs.values().cloned().next().unwrap();
        for input in reissue_req.transaction.inputs.iter() {
            assert_eq!(
                transaction_sigs.get(&input.name()),
                Some(&(pub_key_set.clone(), sig.clone()))
            );
        }
        assert_eq!(transaction_sigs.len(), transaction.inputs.len());

        let mint_sig = genesis_owner
            .public_key_set
            .combine_signatures(vec![sig.threshold_crypto()])
            .unwrap();

        let output_dbcs =
            Vec::from_iter(reissue_req.transaction.outputs.into_iter().map(|content| {
                Dbc {
                    content,
                    transaction: transaction.clone(),
                    transaction_sigs: BTreeMap::from_iter(
                        transaction_sigs
                            .iter()
                            .map(|(input, _)| (*input, (genesis_key, mint_sig.clone()))),
                    ),
                }
            }));

        for dbc in output_dbcs.iter() {
            let expected_amount: u64 = output_amounts[dbc.content.output_number as usize];
            assert_eq!(dbc.amount(), expected_amount);
            assert!(dbc.confirm_valid(&key_manager).is_ok());
        }

        assert_eq!(
            output_dbcs.iter().map(|dbc| dbc.amount()).sum::<u64>(),
            output_amount
        );
    }

    #[test]
    fn test_double_spend_protection() -> Result<()> {
        let genesis_owner = crate::bls_dkg_id();
        let genesis_key = genesis_owner.public_key_set.public_key();
        let key_manager = SimpleKeyManager::new(
            SimpleSigner::new(
                genesis_owner.public_key_set.clone(),
                (0, genesis_owner.secret_key_share.clone()),
            ),
            genesis_owner.public_key_set.public_key(),
        );
        let mut genesis_node = Mint::new(key_manager, SimpleSpendBook::new());

        let (gen_dbc_content, gen_dbc_trans, (gen_key_set, gen_node_sig)) =
            genesis_node.issue_genesis_dbc(1000).unwrap();
        let genesis_sig = gen_key_set
            .combine_signatures(vec![gen_node_sig.threshold_crypto()])
            .unwrap();

        let genesis_dbc = Dbc {
            content: gen_dbc_content,
            transaction: gen_dbc_trans,
            transaction_sigs: BTreeMap::from_iter(vec![(
                GENESIS_DBC_INPUT,
                (genesis_key, genesis_sig),
            )]),
        };

        let inputs = HashSet::from_iter(vec![genesis_dbc.clone()]);
        let input_hashes = BTreeSet::from_iter(vec![genesis_dbc.name()]);

        let transaction = ReissueTransaction {
            inputs: inputs.clone(),
            outputs: HashSet::from_iter(vec![DbcContent::new(
                input_hashes.clone(),
                1000,
                0,
                crate::bls_dkg_id().public_key_set.public_key(),
            )]),
        };

        let sig_share = genesis_node
            .key_manager
            .sign(&transaction.blinded().hash())?;

        let sig = genesis_node
            .key_manager
            .public_key_set()?
            .combine_signatures(vec![sig_share.threshold_crypto()])
            .unwrap();

        let reissue_req = ReissueRequest {
            transaction,
            input_ownership_proofs: HashMap::from_iter(vec![(
                genesis_dbc.name(),
                (genesis_node.key_manager.public_key_set()?.public_key(), sig),
            )]),
        };

        let (t, s) = genesis_node
            .reissue(reissue_req, input_hashes.clone())
            .unwrap();

        let double_spend_transaction = ReissueTransaction {
            inputs,
            outputs: HashSet::from_iter(vec![DbcContent::new(
                input_hashes.clone(),
                1000,
                0,
                crate::bls_dkg_id().public_key_set.public_key(),
            )]),
        };

        let node_share = genesis_node
            .key_manager
            .sign(&double_spend_transaction.blinded().hash())?;

        let sig = genesis_node
            .key_manager
            .public_key_set()?
            .combine_signatures(vec![node_share.threshold_crypto()])
            .unwrap();

        let double_spend_reissue_req = ReissueRequest {
            transaction: double_spend_transaction,
            input_ownership_proofs: HashMap::from_iter(vec![(
                genesis_dbc.name(),
                (genesis_node.key_manager.public_key_set()?.public_key(), sig),
            )]),
        };

        let res = genesis_node.reissue(double_spend_reissue_req, input_hashes);

        println!("res {:?}", res);
        assert!(matches!(
            res,
            Err(Error::DbcAlreadySpent { transaction, transaction_sigs }) if transaction == t && transaction_sigs == s
        ));

        Ok(())
    }

    #[quickcheck]
    fn prop_dbc_transaction_many_to_many(
        // the amount of each input transaction
        input_amounts: TinyVec<TinyInt>,
        // The output_number and amount for each transaction output
        output_amounts: TinyVec<(TinyInt, TinyInt)>,
        // Outputs with output_numbers that appear in this vec will
        // have extra parents inserted into the transaction
        extra_output_parents: TinyVec<TinyInt>,
        // Include a valid ownership proof for the following inputs
        input_owner_proofs: TinyVec<TinyInt>,
        // Include an invalid ownership proof for the following inputs
        invalid_input_owner_proofs: TinyVec<TinyInt>,
    ) {
        let input_amounts = Vec::from_iter(input_amounts.into_iter().map(TinyInt::coerce::<u64>));

        let output_amounts = Vec::from_iter(
            output_amounts
                .into_iter()
                .map(|(number, amount)| (number.coerce::<u32>(), amount.coerce::<u64>())),
        );

        let extra_output_parents =
            Vec::from_iter(extra_output_parents.into_iter().map(TinyInt::coerce::<u32>));

        let inputs_to_create_owner_proofs =
            BTreeSet::from_iter(input_owner_proofs.into_iter().map(TinyInt::coerce::<u32>));

        let inputs_to_create_invalid_owner_proofs = BTreeSet::from_iter(
            invalid_input_owner_proofs
                .into_iter()
                .map(TinyInt::coerce::<u32>),
        );

        let genesis_owner = crate::bls_dkg_id();
        let genesis_key = genesis_owner.public_key_set.public_key();
        let key_manager = SimpleKeyManager::new(
            SimpleSigner::new(
                genesis_owner.public_key_set.clone(),
                (0, genesis_owner.secret_key_share.clone()),
            ),
            genesis_owner.public_key_set.public_key(),
        );
        let mut genesis_node = Mint::new(key_manager, SimpleSpendBook::new());

        let genesis_amount: u64 = input_amounts.iter().sum();
        let (gen_dbc_content, gen_dbc_trans, (_gen_key, gen_node_sig)) =
            genesis_node.issue_genesis_dbc(genesis_amount).unwrap();

        let genesis_sig = genesis_node
            .key_manager
            .public_key_set()
            .unwrap()
            .combine_signatures(vec![gen_node_sig.threshold_crypto()])
            .unwrap();

        let genesis_dbc = Dbc {
            content: gen_dbc_content,
            transaction: gen_dbc_trans,
            transaction_sigs: BTreeMap::from_iter(vec![(
                GENESIS_DBC_INPUT,
                (genesis_key, genesis_sig),
            )]),
        };

        let mut owners: BTreeMap<u32, bls_dkg::outcome::Outcome> = Default::default();

        let gen_inputs = HashSet::from_iter(vec![genesis_dbc.clone()]);
        let gen_input_hashes = BTreeSet::from_iter(gen_inputs.iter().map(Dbc::name));
        let input_content =
            HashSet::from_iter(input_amounts.iter().enumerate().map(|(i, amount)| {
                let owner = crate::bls_dkg_id();
                let owner_public_key = owner.public_key_set.public_key();
                owners.insert(i as u32, owner);
                DbcContent::new(
                    gen_input_hashes.clone(),
                    *amount,
                    i as u32,
                    owner_public_key,
                )
            }));

        let mut reissue_req = ReissueRequest {
            transaction: ReissueTransaction {
                inputs: gen_inputs,
                outputs: input_content.clone(),
            },
            input_ownership_proofs: HashMap::default(),
        };
        let sig_share = genesis_node
            .key_manager
            .sign(&reissue_req.transaction.blinded().hash())
            .unwrap();
        let sig = genesis_node
            .key_manager
            .public_key_set()
            .unwrap()
            .combine_signatures(vec![sig_share.threshold_crypto()])
            .unwrap();
        reissue_req.input_ownership_proofs.insert(
            genesis_dbc.name(),
            (
                genesis_node
                    .key_manager
                    .public_key_set()
                    .unwrap()
                    .public_key(),
                sig,
            ),
        );

        let (transaction, transaction_sigs) =
            genesis_node.reissue(reissue_req, gen_input_hashes).unwrap();

        let (mint_key_set, mint_sig_share) = transaction_sigs.values().cloned().next().unwrap();

        let mint_sig = mint_key_set
            .combine_signatures(vec![mint_sig_share.threshold_crypto()])
            .unwrap();

        let input_dbcs = HashSet::from_iter(input_content.into_iter().map(|content| {
            Dbc {
                content,
                transaction: transaction.clone(),
                transaction_sigs: BTreeMap::from_iter(
                    transaction_sigs
                        .iter()
                        .map(|(input, _)| (*input, (genesis_key, mint_sig.clone()))),
                ),
            }
        }));

        let input_hashes = BTreeSet::from_iter(input_dbcs.iter().map(Dbc::name));

        let outputs = HashSet::from_iter(output_amounts.iter().map(|(output_number, amount)| {
            let mut fuzzed_parents = input_hashes.clone();

            for _ in extra_output_parents
                .iter()
                .filter(|idx| idx == &output_number)
            {
                fuzzed_parents.insert(rand::random());
            }

            let output_owner = crate::bls_dkg_id();
            DbcContent::new(
                fuzzed_parents,
                *amount,
                *output_number,
                output_owner.public_key_set.public_key(),
            )
        }));

        let transaction = ReissueTransaction {
            inputs: input_dbcs,
            outputs: outputs.clone(),
        };

        let transaction_hash = transaction.blinded().hash();

        let mut input_ownership_proofs: HashMap<
            crate::Hash,
            (blsttc::PublicKey, blsttc::Signature),
        > = Default::default();
        input_ownership_proofs.extend(
            inputs_to_create_owner_proofs
                .iter()
                .filter_map(|in_number| {
                    transaction
                        .inputs
                        .iter()
                        .find(|dbc| dbc.content.output_number == *in_number)
                })
                .map(|dbc| {
                    let owner = &owners[&dbc.content.output_number];
                    let sig_share = owner.secret_key_share.sign(&transaction_hash);
                    let owner_key_set = &owner.public_key_set;
                    let sig = owner_key_set
                        .combine_signatures(vec![(0, &sig_share)])
                        .unwrap();

                    (dbc.name(), (owner_key_set.public_key(), sig))
                }),
        );

        input_ownership_proofs.extend(
            inputs_to_create_invalid_owner_proofs
                .iter()
                .filter_map(|in_number| {
                    transaction
                        .inputs
                        .iter()
                        .find(|dbc| dbc.content.output_number == *in_number)
                })
                .map(|dbc| {
                    let random_owner = crate::bls_dkg_id();
                    let sig_share = random_owner.secret_key_share.sign(&transaction_hash);
                    let owner_key_set = random_owner.public_key_set;
                    let sig = owner_key_set
                        .combine_signatures(vec![(0, &sig_share)])
                        .unwrap();

                    (dbc.name(), (owner_key_set.public_key(), sig))
                }),
        );

        let reissue_req = ReissueRequest {
            transaction,
            input_ownership_proofs,
        };

        let many_to_many_result = genesis_node.reissue(reissue_req, input_hashes);

        let output_amount: u64 = outputs.iter().map(|output| output.amount).sum();
        let number_of_fuzzed_output_parents = BTreeSet::from_iter(extra_output_parents)
            .intersection(&BTreeSet::from_iter(output_amounts.iter().map(|(n, _)| *n)))
            .count();

        match many_to_many_result {
            Ok((transaction, transaction_sigs)) => {
                assert_eq!(genesis_amount, output_amount);
                assert_eq!(number_of_fuzzed_output_parents, 0);
                assert!(
                    input_amounts.is_empty()
                        || inputs_to_create_invalid_owner_proofs
                            .intersection(&BTreeSet::from_iter(owners.keys().copied()))
                            .next()
                            .is_none()
                );
                assert!(BTreeSet::from_iter(owners.keys().copied())
                    .is_subset(&inputs_to_create_owner_proofs));

                // The output amounts should correspond to the output_amounts
                assert_eq!(
                    BTreeSet::from_iter(outputs.iter().map(|o| o.amount)),
                    BTreeSet::from_iter(output_amounts.into_iter().map(|(_, a)| a))
                );

                // The outputs should have been uniquely number from 0 to N (N = # of outputs)
                assert_eq!(
                    BTreeSet::from_iter(
                        outputs.iter().map(|content| content.output_number as usize)
                    ),
                    BTreeSet::from_iter(0..outputs.len())
                );

                let (mint_key_set, mint_sig_share) = transaction_sigs.values().next().unwrap();
                let mint_sig = mint_key_set
                    .combine_signatures(vec![mint_sig_share.threshold_crypto()])
                    .unwrap();

                let output_dbcs = Vec::from_iter(outputs.into_iter().map(|content| {
                    Dbc {
                        content,
                        transaction: transaction.clone(),
                        transaction_sigs: BTreeMap::from_iter(
                            transaction_sigs
                                .iter()
                                .map(|(input, _)| (*input, (genesis_key, mint_sig.clone()))),
                        ),
                    }
                }));

                for dbc in output_dbcs.iter() {
                    let dbc_confirm_result = dbc.confirm_valid(&genesis_node.key_manager);
                    println!("DBC confirm result {:?}", dbc_confirm_result);
                    assert!(dbc_confirm_result.is_ok());
                }

                assert_eq!(
                    output_dbcs.iter().map(|dbc| dbc.amount()).sum::<u64>(),
                    output_amount
                );
            }
            Err(Error::DbcReissueRequestDoesNotBalance { .. }) => {
                assert_ne!(genesis_amount, output_amount);
            }
            Err(Error::TransactionMustHaveAnInput) => {
                assert_eq!(input_amounts.len(), 0);
            }
            Err(Error::OutputsAreNotNumberedCorrectly) => {
                assert_ne!(
                    BTreeSet::from_iter(
                        outputs.iter().map(|content| content.output_number as usize)
                    ),
                    BTreeSet::from_iter(0..outputs.len())
                );
            }
            Err(Error::DbcContentParentsDifferentFromTransactionInputs) => {
                assert_ne!(number_of_fuzzed_output_parents, 0)
            }
            Err(Error::MissingInputOwnerProof) => {
                assert!(!BTreeSet::from_iter(owners.keys().copied())
                    .is_subset(&inputs_to_create_owner_proofs));
            }
            Err(Error::FailedSignature) => {
                assert_ne!(inputs_to_create_invalid_owner_proofs.len(), 0);
            }
            Err(Error::FailedUnblinding) => {
                assert_ne!(inputs_to_create_invalid_owner_proofs.len(), 0);
            }
            err => panic!("Unexpected reissue err {:#?}", err),
        }
    }

    #[quickcheck]
    #[ignore]
    fn prop_in_progress_transaction_can_be_continued_across_churn() {
        todo!()
    }

    #[quickcheck]
    #[ignore]
    fn prop_reject_invalid_prefix() {
        todo!();
    }

    #[test]
    fn test_inputs_are_validated() {
        let genesis_owner = crate::bls_dkg_id();
        let key_manager = SimpleKeyManager::new(
            SimpleSigner::new(
                genesis_owner.public_key_set.clone(),
                (0, genesis_owner.secret_key_share.clone()),
            ),
            genesis_owner.public_key_set.public_key(),
        );
        let mut genesis_node = Mint::new(key_manager, SimpleSpendBook::new());

        let input_owner = crate::bls_dkg_id();
        let input_content = DbcContent::new(
            Default::default(),
            100,
            0,
            input_owner.public_key_set.public_key(),
        );
        let input_content_hashes = BTreeSet::from_iter(vec![input_content.hash()]);

        let fraudulant_reissue_result = genesis_node.reissue(
            ReissueRequest {
                transaction: ReissueTransaction {
                    inputs: HashSet::from_iter(vec![Dbc {
                        content: input_content,
                        transaction: DbcTransaction {
                            inputs: Default::default(),
                            outputs: input_content_hashes.clone(),
                        },
                        transaction_sigs: Default::default(),
                    }]),
                    outputs: HashSet::from_iter(vec![DbcContent::new(
                        input_content_hashes.clone(),
                        100,
                        0,
                        crate::bls_dkg_id().public_key_set.public_key(),
                    )]),
                },
                input_ownership_proofs: HashMap::default(),
            },
            input_content_hashes,
        );
        assert!(fraudulant_reissue_result.is_err());
    }
}
