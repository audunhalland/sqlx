use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use syn::Path;

use sqlx::describe::Describe;

use crate::database::DatabaseExt;

use crate::query_macros::data::QueryData;
use std::fmt::{self, Display, Formatter};

pub struct RustColumn {
    pub(super) ident: Ident,
    pub(super) type_: TokenStream,
}

pub fn columns_to_rust<DB: DatabaseExt>(query_data: &QueryData) -> crate::Result<Vec<RustColumn>> {
    query_data
        .outputs
        .iter()
        .enumerate()
        .map(|(i, (col_name, col_type))| -> crate::Result<_> {
            let ident = parse_ident(col_name)?;
            Ok(RustColumn {
                ident,
                type_: col_type
                    .parse()
                    .map_err(|_| panic!("failed to parse {:?} as a Rust type", col_type))?,
            })
        })
        .collect::<crate::Result<Vec<_>>>()
}

pub fn quote_query_as<DB: DatabaseExt>(
    sql: &str,
    out_ty: &Path,
    bind_args: &Ident,
    columns: &[RustColumn],
    checked: bool,
) -> TokenStream {
    let instantiations = columns.iter().enumerate().map(
        |(
            i,
            &RustColumn {
                ref ident,
                ref type_,
                ..
            },
        )| {
            // For "checked" queries, the macro checks these at compile time and using "try_get"
            // would also perform pointless runtime checks

            if checked {
                quote!( #ident: row.try_get_unchecked::<#type_, _>(#i).try_unwrap_optional()? )
            } else {
                quote!( #ident: row.try_get_unchecked(#i)? )
            }
        },
    );

    let db_path = DB::db_path();
    let row_path = DB::row_path();

    quote! {
        sqlx::query::<#db_path>(#sql).bind_all(#bind_args).try_map(|row: #row_path| {
            use sqlx::Row as _;
            use sqlx::result_ext::ResultExt as _;

            Ok(#out_ty { #(#instantiations),* })
        })
    }
}

fn parse_ident(name: &str) -> crate::Result<Ident> {
    // workaround for the following issue (it's semi-fixed but still spits out extra diagnostics)
    // https://github.com/dtolnay/syn/issues/749#issuecomment-575451318

    let is_valid_ident = name.chars().all(|c| c.is_alphanumeric() || c == '_');

    if is_valid_ident {
        let ident = String::from("r#") + name;
        if let Ok(ident) = syn::parse_str(&ident) {
            return Ok(ident);
        }
    }

    Err(format!("{:?} is not a valid Rust identifier", name).into())
}
