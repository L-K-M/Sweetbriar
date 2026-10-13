extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn store_attr(_: TokenStream, input: TokenStream) -> TokenStream {
    input
}

#[proc_macro_derive(Store)]
pub fn store_derive(_: TokenStream) -> TokenStream {
    TokenStream::new()
}
