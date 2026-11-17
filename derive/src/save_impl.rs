//! `#[derive(Save)]`: picks a type's save rule from its fields. See `reacs::save`.

use proc_macro2::{TokenStream, TokenTree};
use quote::{format_ident, quote, quote_spanned};
use syn::{Data, DeriveInput, Fields, Index, Member, Type, parse_quote, spanned::Spanned};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Saved by the field type's own `Save`.
    Own,
    /// `#[save(plain)]`: the field's memory, copied as it is.
    Plain,
    /// `#[save(serde)]`: encoded with serde.
    Serde,
    /// `#[save(skip)]` or `#[serde(skip)]`: not saved; `Default` on load.
    Skip,
}

struct Field {
    member: Member,
    ty: Type,
    mode: Mode,
}

fn field_mode(attrs: &[syn::Attribute]) -> syn::Result<Mode> {
    let mut mode = Mode::Own;
    for attr in attrs {
        if attr.path().is_ident("save") {
            attr.parse_nested_meta(|meta| {
                mode = if meta.path.is_ident("plain") {
                    Mode::Plain
                } else if meta.path.is_ident("serde") {
                    Mode::Serde
                } else if meta.path.is_ident("skip") {
                    Mode::Skip
                } else {
                    return Err(meta.error("expected `plain`, `serde` or `skip`"));
                };
                Ok(())
            })?;
        } else if attr.path().is_ident("serde")
            && let syn::Meta::List(list) = &attr.meta
            && list
                .tokens
                .clone()
                .into_iter()
                .any(|token| matches!(token, TokenTree::Ident(ident) if ident == "skip"))
        {
            mode = Mode::Skip;
        }
    }
    Ok(mode)
}

fn fields_of(fields: &Fields) -> syn::Result<Vec<Field>> {
    fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            Ok(Field {
                member: match &field.ident {
                    Some(ident) => Member::Named(ident.clone()),
                    None => Member::Unnamed(Index::from(i)),
                },
                ty: field.ty.clone(),
                mode: field_mode(&field.attrs)?,
            })
        })
        .collect()
}

/// Writes one field, given a reference to it.
fn save_field(field: &Field, value: TokenStream) -> TokenStream {
    let ty = &field.ty;
    match field.mode {
        Mode::Own => quote!(<#ty as ::reacs::save::Save>::save(#value, out);),
        // Spanned on the type, so a non-`Copy` type is reported at the field.
        Mode::Plain => quote_spanned!(ty.span()=> ::reacs::save::save_plain::<#ty>(#value, out);),
        Mode::Serde => quote_spanned!(ty.span()=> ::reacs::save::save_serde::<#ty>(#value, out);),
        Mode::Skip => quote!(),
    }
}

/// Reads one field.
fn load_field(field: &Field) -> TokenStream {
    let ty = &field.ty;
    match field.mode {
        Mode::Own => quote!(<#ty as ::reacs::save::Save>::load(reader)),
        Mode::Plain => quote_spanned!(ty.span()=> reader.load_plain::<#ty>()),
        Mode::Serde => quote_spanned!(ty.span()=> reader.load_serde::<#ty>()),
        Mode::Skip => quote!(::core::default::Default::default()),
    }
}

pub(crate) fn derive(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let mut generics = input.generics.clone();
    for param in generics.type_params_mut() {
        param.bounds.push(parse_quote!(::reacs::save::Save));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let body = match &input.data {
        Data::Struct(data) => derive_struct(&fields_of(&data.fields)?),
        Data::Enum(data) => derive_enum(data)?,
        Data::Union(data) => {
            return Err(syn::Error::new(
                data.union_token.span(),
                "#[derive(Save)] doesn't support unions",
            ));
        }
    };

    Ok(quote! {
        impl #impl_generics ::reacs::save::Save for #name #ty_generics #where_clause {
            #body
        }
    })
}

fn derive_struct(fields: &[Field]) -> TokenStream {
    let opaque = fields
        .iter()
        .any(|f| matches!(f.mode, Mode::Serde | Mode::Skip));

    // Plain when every field is, and they sit in declaration order with no gaps: then the memory
    // is the fields' encodings back to back, which is what field by field writes.
    let plain = if opaque {
        quote!(false)
    } else {
        let own = fields.iter().filter(|f| f.mode == Mode::Own).map(|f| &f.ty);
        let layout = fields.iter().map(|f| {
            let member = &f.member;
            let ty = &f.ty;
            quote! {
                plain = plain && ::core::mem::offset_of!(Self, #member) == end;
                end += ::core::mem::size_of::<#ty>();
            }
        });
        quote! {{
            #[allow(unused_mut)]
            let mut plain = true #(&& <#own as ::reacs::save::Save>::PLAIN)*;
            #[allow(unused_mut)]
            let mut end = 0;
            #(#layout)*
            plain && ::core::mem::size_of::<Self>() == end
        }}
    };

    let own = fields.iter().filter(|f| f.mode == Mode::Own).map(|f| &f.ty);
    let checked = quote!(false #(|| <#own as ::reacs::save::Save>::CHECKED)*);

    let checks = fields.iter().filter(|f| f.mode == Mode::Own).map(|f| {
        let ty = &f.ty;
        let member = &f.member;
        quote! {
            && <#ty as ::reacs::save::Save>::check_all(
                bytes, count, stride, offset + ::core::mem::offset_of!(Self, #member),
            )
        }
    });
    let saves = fields.iter().map(|f| {
        let member = &f.member;
        save_field(f, quote!(&self.#member))
    });
    let loads = fields.iter().map(|f| {
        let member = &f.member;
        let load = load_field(f);
        quote!(#member: #load,)
    });

    quote! {
        const PLAIN: bool = #plain;
        const CHECKED: bool = #checked;

        #[inline]
        fn save(&self, out: &mut ::std::vec::Vec<u8>) {
            if <Self as ::reacs::save::Save>::PLAIN {
                out.extend_from_slice(::reacs::save::memory_of(self));
            } else {
                #(#saves)*
            }
        }

        #[inline]
        fn load(reader: &mut ::reacs::save::Reader<'_>) -> Self {
            if <Self as ::reacs::save::Save>::PLAIN {
                reader.load_memory::<Self>()
            } else {
                Self { #(#loads)* }
            }
        }

        #[inline]
        fn check_all(bytes: &[u8], count: usize, stride: usize, offset: usize) -> bool {
            let _ = (bytes, count, stride, offset);
            true #(#checks)*
        }
    }
}

fn derive_enum(data: &syn::DataEnum) -> syn::Result<TokenStream> {
    let fieldless = !data.variants.is_empty()
        && data
            .variants
            .iter()
            .all(|variant| matches!(variant.fields, Fields::Unit));

    if fieldless {
        // Its memory is its tag, which must be one of the variants: plain, checked.
        let variants: Vec<_> = data.variants.iter().map(|v| &v.ident).collect();
        let indexes = 0..variants.len();
        return Ok(quote! {
            const PLAIN: bool =
                ::core::cfg!(target_endian = "little") || ::core::mem::size_of::<Self>() <= 1;
            const CHECKED: bool = true;

            #[inline]
            fn save(&self, out: &mut ::std::vec::Vec<u8>) {
                ::reacs::save::save_tag(self, out);
            }

            #[inline]
            fn load(reader: &mut ::reacs::save::Reader<'_>) -> Self {
                match ::reacs::save::load_tag(reader, &[#(Self::#variants),*]) {
                    #(#indexes => Self::#variants,)*
                    _ => ::core::unreachable!(),
                }
            }

            #[inline]
            fn check_all(bytes: &[u8], count: usize, stride: usize, offset: usize) -> bool {
                ::reacs::save::check_tags(&[#(Self::#variants),*], bytes, count, stride, offset)
            }
        });
    }

    // Field by field: the variant's index as a `u32`, then its fields.
    let mut save_arms = Vec::new();
    let mut load_arms = Vec::new();
    for (index, variant) in data.variants.iter().enumerate() {
        let ident = &variant.ident;
        let index = index as u32;
        let fields = fields_of(&variant.fields)?;
        let bindings: Vec<_> = (0..fields.len()).map(|i| format_ident!("f{i}")).collect();
        let members = fields.iter().map(|field| &field.member);
        let saves = fields
            .iter()
            .zip(&bindings)
            .map(|(field, binding)| save_field(field, quote!(#binding)));
        let loads = fields.iter().map(|field| {
            let member = &field.member;
            let load = load_field(field);
            quote!(#member: #load,)
        });
        save_arms.push(quote! {
            Self::#ident { #(#members: #bindings,)* } => {
                <u32 as ::reacs::save::Save>::save(&#index, out);
                #(#saves)*
            }
        });
        load_arms.push(quote!(#index => Self::#ident { #(#loads)* },));
    }

    Ok(quote! {
        const PLAIN: bool = false;

        #[inline]
        #[allow(unused_variables)]
        fn save(&self, out: &mut ::std::vec::Vec<u8>) {
            match self {
                #(#save_arms)*
            }
        }

        #[inline]
        fn load(reader: &mut ::reacs::save::Reader<'_>) -> Self {
            match <u32 as ::reacs::save::Save>::load(reader) {
                #(#load_arms)*
                _ => ::core::panic!(
                    "Failed to load save, it's corrupted: bad `{}`",
                    ::core::any::type_name::<Self>(),
                ),
            }
        }
    })
}
