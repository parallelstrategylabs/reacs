use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    Fields, GenericArgument, GenericParam, Ident, Index, ItemStruct, Lifetime, Member,
    PathArguments, Type, TypeParam, TypeParamBound,
    parse::{Parse, ParseStream},
    parse_quote,
    spanned::Spanned,
};

#[derive(Clone, Copy, PartialEq)]
enum Modes {
    Both,
    Read,
    Write,
}

impl Parse for Modes {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Modes::Both);
        }
        let id: Ident = input.parse()?;
        match id.to_string().as_str() {
            "read" => Ok(Modes::Read),
            "write" => Ok(Modes::Write),
            _ => Err(syn::Error::new(id.span(), "expected `read` or `write`")),
        }
    }
}

#[derive(Clone, Copy)]
enum Pin {
    Read,
    Write,
}

struct FieldInfo {
    member: Member,
    pin: Option<Pin>,
    ty: Type,
}

fn lt(s: &str) -> Lifetime {
    Lifetime::new(s, Span::call_site())
}

fn mode_of(pin: Option<Pin>, default: &Type) -> Type {
    match pin {
        Some(Pin::Read) => parse_quote!(::reacs::schedule::Read),
        Some(Pin::Write) => parse_quote!(::reacs::schedule::Write),
        None => default.clone(),
    }
}

fn add_static(tp: &mut TypeParam) {
    let has = tp
        .bounds
        .iter()
        .any(|b| matches!(b, TypeParamBound::Lifetime(l) if l.ident == "static"));
    if !has {
        tp.bounds.push(parse_quote!('static));
    }
}

fn leaf_ident(ty: &Type, params: &[Ident]) -> Option<Ident> {
    match ty {
        Type::Path(p) if p.qself.is_none() => {
            p.path.get_ident().filter(|i| params.contains(i)).cloned()
        }
        _ => None,
    }
}

fn rewrite_path(ty: &Type, lt: &Lifetime, mode: &Type) -> syn::Result<Type> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new(
            ty.span(),
            "#[param] fields must be a system param type",
        ));
    };
    let mut path = path.clone();
    let seg = path.path.segments.last_mut().expect("path has a segment");
    if let PathArguments::AngleBracketed(ab) = &mut seg.arguments {
        ab.args.insert(0, GenericArgument::Lifetime(lt.clone()));
        ab.args.push(GenericArgument::Type(mode.clone()));
    } else if matches!(seg.arguments, PathArguments::None) {
        seg.arguments = PathArguments::AngleBracketed(parse_quote!(<#lt, #mode>));
    } else {
        return Err(syn::Error::new(ty.span(), "unsupported field type"));
    }
    Ok(Type::Path(path))
}

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let modes: Modes = syn::parse2(attr)?;
    let mut input: ItemStruct = syn::parse2(item)?;
    let name = input.ident.clone();

    if input.generics.lifetimes().next().is_some() {
        return Err(syn::Error::new(
            input.generics.span(),
            "#[param] adds the `'w` lifetime itself, don't declare one",
        ));
    }
    if input.generics.type_params().any(|t| t.ident == "M") {
        return Err(syn::Error::new(
            input.generics.span(),
            "#[param] uses `M` for the mode parameter, rename your type parameter",
        ));
    }

    let type_params: Vec<Ident> = input
        .generics
        .type_params()
        .map(|t| t.ident.clone())
        .collect();

    let user_args: Vec<TokenStream> = input
        .generics
        .params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type(t) => {
                let i = &t.ident;
                Some(quote!(#i))
            }
            GenericParam::Const(c) => {
                let i = &c.ident;
                Some(quote!(#i))
            }
            GenericParam::Lifetime(_) => None,
        })
        .collect();

    let w = lt("'w");
    let m: Type = parse_quote!(M);

    let mut infos: Vec<FieldInfo> = Vec::new();
    {
        let fields = match &mut input.fields {
            Fields::Named(f) => &mut f.named,
            Fields::Unnamed(f) => &mut f.unnamed,
            Fields::Unit => {
                return Err(syn::Error::new(
                    name.span(),
                    "#[param] needs at least one field",
                ));
            }
        };
        if fields.is_empty() {
            return Err(syn::Error::new(
                name.span(),
                "#[param] needs at least one field",
            ));
        }

        for (i, field) in fields.iter_mut().enumerate() {
            let member = match &field.ident {
                Some(id) => Member::Named(id.clone()),
                None => Member::Unnamed(Index::from(i)),
            };

            let mut pin = None;
            field.attrs.retain(|a| {
                if a.path().is_ident("read") {
                    pin = Some(Pin::Read);
                    false
                } else if a.path().is_ident("write") {
                    pin = Some(Pin::Write);
                    false
                } else {
                    true
                }
            });

            let ty = field.ty.clone();
            if let Some(t) = leaf_ident(&ty, &type_params) {
                return Err(syn::Error::new(
                    ty.span(),
                    format!(
                        "`{t}` is a bare type parameter. Every field of a #[param] must be \
                         another param, e.g. `Resource<{t}>`"
                    ),
                ));
            }

            let info = FieldInfo { member, pin, ty };
            field.ty = rewrite_path(&info.ty, &w, &mode_of(info.pin, &m))?;
            infos.push(info);
        }
    }

    if infos.iter().all(|i| i.pin.is_some()) {
        return Err(syn::Error::new(
            name.span(),
            "at least one field must follow the mode (not be pinned with #[read]/#[write]), \
             otherwise `M` is unused",
        ));
    }

    let mut base = input.generics.clone();
    for tp in base.type_params_mut() {
        add_static(tp);
    }

    let mut struct_generics = base.clone();
    struct_generics.params.insert(0, parse_quote!('w));
    struct_generics
        .params
        .push(parse_quote!(M: ::reacs::schedule::Mode = ::reacs::schedule::Read));
    input.generics = struct_generics;

    let ty_with = |lt: &Lifetime, mode: &Type| quote!(#name<#lt, #(#user_args,)* #mode>);

    let with_mode = if modes != Modes::Read {
        let mut g = base.clone();
        g.params.insert(0, parse_quote!('w));
        g.params.push(parse_quote!(__M0: ::reacs::schedule::Mode));
        g.params.push(parse_quote!(__M: ::reacs::schedule::Mode));
        let (impl_g, _, where_g) = g.split_for_impl();
        let m0: Type = parse_quote!(__M0);
        let m1: Type = parse_quote!(__M);
        let from = ty_with(&w, &m0);
        let to = ty_with(&w, &m1);
        Some(quote! {
            impl #impl_g ::reacs::schedule::WithMode<__M> for #from #where_g {
                type Out = #to;
            }
        })
    } else {
        None
    };

    let deref = if infos.len() == 1 {
        let info = &infos[0];
        let f_ty = rewrite_path(&info.ty, &w, &mode_of(info.pin, &m))?;
        let member = &info.member;
        let target: Type = parse_quote!(<#f_ty as ::core::ops::Deref>::Target);

        let mut dg = base.clone();
        dg.params.insert(0, parse_quote!('w));
        dg.params.push(parse_quote!(M: ::reacs::schedule::Mode));

        let mut deref_g = dg.clone();
        deref_g
            .make_where_clause()
            .predicates
            .push(parse_quote!(#f_ty: ::core::ops::Deref));

        let mut deref_mut_g = dg;
        deref_mut_g
            .make_where_clause()
            .predicates
            .push(parse_quote!(#f_ty: ::core::ops::DerefMut));

        let (di, _, dw) = deref_g.split_for_impl();
        let (dmi, _, dmw) = deref_mut_g.split_for_impl();
        let self_ty = ty_with(&w, &m);

        Some(quote! {
            impl #di ::core::ops::Deref for #self_ty #dw {
                type Target = #target;
                fn deref(&self) -> &Self::Target {
                    &*self.#member
                }
            }

            impl #dmi ::core::ops::DerefMut for #self_ty #dmw {
                fn deref_mut(&mut self) -> &mut Self::Target {
                    &mut *self.#member
                }
            }
        })
    } else {
        None
    };

    let (sp_mode, sp_mode_param): (Type, Option<GenericParam>) = match modes {
        Modes::Both => (
            parse_quote!(M),
            Some(parse_quote!(M: ::reacs::schedule::Mode)),
        ),
        Modes::Read => (parse_quote!(::reacs::schedule::Read), None),
        Modes::Write => (parse_quote!(::reacs::schedule::Write), None),
    };

    let mut sp_g = base.clone();
    if let Some(p) = sp_mode_param {
        sp_g.params.push(p);
    }
    let (sp_impl, _, sp_where) = sp_g.split_for_impl();

    let infer = lt("'_");
    let fetch_lt = lt("'__w");

    let mut init_stmts: Vec<TokenStream> = Vec::new();
    let mut fetch_exprs: Vec<TokenStream> = Vec::new();
    let mut members: Vec<&Member> = Vec::new();

    for info in &infos {
        let mode = mode_of(info.pin, &sp_mode);
        members.push(&info.member);
        let init_ty = rewrite_path(&info.ty, &infer, &mode)?;
        let fetch_ty = rewrite_path(&info.ty, &fetch_lt, &mode)?;
        init_stmts.push(quote!(
            <#init_ty as ::reacs::schedule::SystemParam>::init(world.reborrow());
        ));
        fetch_exprs.push(quote!(
            <#fetch_ty as ::reacs::schedule::SystemParam>::fetch(world)
        ));
    }

    let sp_header = ty_with(&infer, &sp_mode);
    let sp_item = ty_with(&fetch_lt, &sp_mode);

    let system_param = quote! {
        impl #sp_impl ::reacs::schedule::SystemParam for #sp_header #sp_where {
            type Item<'__w> = #sp_item;

            #[allow(unused_mut, unused_variables)]
            fn init(mut world: ::reacs::schedule::ParamInit<'_>) {
                #(#init_stmts)*
            }

            fn fetch<'__w>(world: ::reacs::schedule::ParamWorld<'__w>) -> #sp_item {
                #name { #(#members: #fetch_exprs),* }
            }
        }
    };

    Ok(quote! {
        #input
        #with_mode
        #deref
        #system_param
    })
}
