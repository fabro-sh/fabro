use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Data, DeriveInput, Fields, Ident, ItemFn, LitStr, Token, parenthesized, parse_macro_input,
};

enum E2eRequirement {
    Live(LitStr),
}

impl Parse for E2eRequirement {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let ident = input.parse::<Ident>()?;
        if ident != "live" {
            return Err(syn::Error::new(
                ident.span(),
                "expected `live(\"ENV_VAR\")`",
            ));
        }

        let content;
        parenthesized!(content in input);
        let env_var = content.parse::<LitStr>()?;
        if !content.is_empty() {
            return Err(content.error("expected a single string literal"));
        }
        Ok(Self::Live(env_var))
    }
}

#[proc_macro_derive(Combine)]
pub fn derive_combine(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let body = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => {
                let combined = fields.named.into_iter().map(|field| {
                    let ident = field.ident.expect("named field");
                    quote! {
                        #ident: ::fabro_types::combine::Combine::combine(self.#ident, other.#ident)
                    }
                });
                quote! {
                    Self {
                        #(#combined,)*
                    }
                }
            }
            Fields::Unnamed(fields) => {
                let combined = fields.unnamed.iter().enumerate().map(|(index, _)| {
                    let index = syn::Index::from(index);
                    quote! {
                        ::fabro_types::combine::Combine::combine(self.#index, other.#index)
                    }
                });
                quote! {
                    Self(#(#combined),*)
                }
            }
            Fields::Unit => quote!(Self),
        },
        Data::Enum(_) | Data::Union(_) => {
            quote!(self)
        }
    };

    quote! {
        impl #impl_generics ::fabro_types::combine::Combine for #ident #ty_generics #where_clause {
            fn combine(self, other: Self) -> Self {
                #body
            }
        }
    }
    .into()
}

#[proc_macro_attribute]
pub fn e2e_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let requirements =
        parse_macro_input!(attr with Punctuated::<E2eRequirement, Token![,]>::parse_terminated);
    let input = parse_macro_input!(item as ItemFn);

    let attrs = input.attrs;
    let vis = input.vis;
    let sig = input.sig;
    let block = input.block;

    let env_vars: Vec<_> = requirements
        .into_iter()
        .map(|requirement| match requirement {
            E2eRequirement::Live(env_var) => env_var,
        })
        .collect();

    let ignore_reason = if env_vars.is_empty() {
        "e2e".to_string()
    } else {
        let joined = env_vars
            .iter()
            .map(LitStr::value)
            .collect::<Vec<_>>()
            .join(", ");
        format!("e2e: {joined}")
    };

    let test_attr = if sig.asyncness.is_some() {
        quote!(#[tokio::test])
    } else {
        quote!(#[test])
    };

    let env_guards = env_vars.iter().map(|env_var| {
        let env_name = env_var.value();
        let strict_message = format!("{env_name} not set (FABRO_TEST_MODE=strict)");
        let skip_message = format!("skipping: {env_name} not set");
        quote! {
            if ::std::env::var(#env_var).is_err() {
                if __mode == ::fabro_test::TestMode::Strict {
                    panic!(#strict_message);
                }
                eprintln!(#skip_message);
                return;
            }
        }
    });

    quote! {
        #(#attrs)*
        #test_attr
        #[ignore = #ignore_reason]
        #vis #sig {
            let __mode = ::fabro_test::TestMode::from_env();

            if __mode == ::fabro_test::TestMode::Off {
                eprintln!("skipping: FABRO_TEST_MODE is off");
                return;
            }

            #(#env_guards)*

            #block
        }
    }
    .into()
}
