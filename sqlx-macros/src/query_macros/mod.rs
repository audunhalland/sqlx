use std::fmt::Display;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

pub use input::QueryMacroInput;
pub use query::expand_query;

use crate::database::DatabaseExt;

use sqlx::connection::Connection;
use sqlx::database::Database;

mod args;
mod data;
mod input;
mod output;
mod query;

pub fn expand_input(mut input: QueryMacroInput) -> crate::Result<TokenStream> {
    let source = input.src.resolve(input.src_span)?;
}

/// Run a parse/describe on the query described by this input and validate that it matches the
/// passed number of args
async fn describe_validate<C: Connection>(
    query: &str,
    conn: &mut C,
) -> crate::Result<Describe<C::Database>> {
    let describe = conn
        .describe(query)
        .await
        .map_err(|e| syn::Error::new(self.source_span, e))?;

    if self.arg_names.len() != describe.param_types.len() {
        return Err(syn::Error::new(
            Span::call_site(),
            format!(
                "expected {} parameters, got {}",
                describe.param_types.len(),
                self.arg_names.len()
            ),
        )
        .into());
    }

    Ok(describe)
}

pub async fn expand_query_file<C: Connection>(
    input: QueryMacroInput,
    conn: C,
) -> crate::Result<TokenStream>
where
    C::Database: DatabaseExt + Sized,
    <C::Database as Database>::TypeInfo: Display,
{
    expand_query(input.expand_file_src().await?, conn).await
}

pub async fn expand_query_as<C: Connection>(
    input: QueryAsMacroInput,
    mut conn: C,
    checked: bool,
) -> crate::Result<TokenStream>
where
    C::Database: DatabaseExt + Sized,
    <C::Database as Database>::TypeInfo: Display,
{
    let describe = input.query_input.describe_validate(&mut conn).await?;

    if describe.result_columns.is_empty() {
        return Err(syn::Error::new(
            input.query_input.source_span,
            "query must output at least one column",
        )
        .into());
    }

    let args_tokens = args::quote_args(&input.query_input, &describe, checked)?;

    let query_args = format_ident!("query_args");

    let columns = output::columns_to_rust(&describe)?;
    let output = output::quote_query_as::<C::Database>(
        &input.query_input.source,
        &input.as_ty.path,
        &query_args,
        &columns,
        checked,
    );

    let arg_names = &input.query_input.arg_names;

    Ok(quote! {
        macro_rules! macro_result {
            (#($#arg_names:expr),*) => {{
                use sqlx::arguments::Arguments as _;

                #args_tokens

                #output
            }}
        }
    })
}

pub async fn expand_query_file_as<C: Connection>(
    input: QueryAsMacroInput,
    conn: C,
    checked: bool,
) -> crate::Result<TokenStream>
where
    C::Database: DatabaseExt + Sized,
    <C::Database as Database>::TypeInfo: Display,
{
    expand_query_as(input.expand_file_src().await?, conn, checked).await
}
