use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, DeriveInput};
use uuid::Uuid;

fn generate_uuid_v5(struct_name: &str) -> u128 {
    let engine_namespace = Uuid::parse_str("804277dc-701f-41c1-b670-fbf99628435f").unwrap();
    let uuid = Uuid::new_v5(&engine_namespace, struct_name.as_bytes());
    uuid.as_u128()
}

#[proc_macro_derive(Hashed)]
pub fn derive_hashed_def(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let name_str = name.to_string();
    let hash_value = generate_uuid_v5(&name_str);

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let expanded = quote! {
        impl #impl_generics ::reacs::Hashed for #name #ty_generics #where_clause {
            const TYPE_UUID: u128 = #hash_value;
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn resource(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;
    let snake_name = format_ident!("{}", name.to_string().to_lowercase());
    let ctor_fn = format_ident!("__register_{}", snake_name);

    let expanded = quote! {
        #[derive(::core::default::Default, ::reacs::Hashed, ::serde::Serialize, ::serde::Deserialize)]
        #input

        #[::ctor::ctor]
        fn #ctor_fn() {
            ::reacs::registry::register_entry::<#name>(#name::TYPE_UUID);
        }
    };

    TokenStream::from(expanded)
}