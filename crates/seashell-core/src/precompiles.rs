use agave_precompiles::get_precompiles;
use solana_account::{AccountSharedData, WritableAccount};

use crate::Seashell;

pub fn load(seashell: &mut Seashell) {
    for precompile in get_precompiles() {
        if precompile
            .feature
            .is_none_or(|feature_id| seashell.feature_set.is_active(&feature_id))
        {
            let mut account_shared_data =
                AccountSharedData::new(1, 0, &solana_sdk_ids::native_loader::id());
            account_shared_data.set_executable(true);
            seashell
                .accounts_db
                .set_account(precompile.program_id, account_shared_data);
        }
    }
}
