use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{DeriveInput, Ident, ItemFn, LitStr, Token, parenthesized, parse_macro_input};

enum E2eRequirement {
    Twin,
    Live(LitStr),
}

impl Parse for E2eRequirement {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let ident = input.parse::<Ident>()?;
        if ident == "twin" {
            return Ok(Self::Twin);
        }
        if ident == "live" {
            let content;
            parenthesized!(content in input);
            let env_var = content.parse::<LitStr>()?;
            if !content.is_empty() {
                return Err(content.error("expected a single string literal"));
            }
            return Ok(Self::Live(env_var));
        }
        Err(syn::Error::new(
            ident.span(),
            "expected `twin` or `live(\"ENV_VAR\")`",
        ))
    }
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

    let has_twin = requirements
        .iter()
        .any(|r| matches!(r, E2eRequirement::Twin));
    let env_vars: Vec<_> = requirements
        .iter()
        .filter_map(|r| match r {
            E2eRequirement::Live(env_var) => Some(env_var.clone()),
            E2eRequirement::Twin => None,
        })
        .collect();
    let has_live = !env_vars.is_empty();

    // Build ignore reason
    let ignore_reason = if !has_twin && !has_live {
        "e2e".to_string()
    } else {
        let mut parts = Vec::new();
        if has_twin {
            parts.push("twin".to_string());
        }
        for var in &env_vars {
            parts.push(var.value());
        }
        format!("e2e: {}", parts.join(", "))
    };

    let test_attr = if sig.asyncness.is_some() {
        quote!(#[tokio::test])
    } else {
        quote!(#[test])
    };

    // Mode-based gating
    let mode_guard = if has_twin && !has_live {
        // twin-only: skip in live/strict
        quote! {
            if __mode.is_live() {
                eprintln!("skipping: twin-only test");
                return;
            }
        }
    } else if !has_twin && has_live {
        // live-only: skip in twin
        quote! {
            if __mode.is_twin() {
                eprintln!("skipping: live-only test");
                return;
            }
        }
    } else {
        // bare or dual: no mode skip
        quote! {}
    };

    // Env var guards (only checked when in live/strict mode)
    let env_guards = if env_vars.is_empty() {
        quote! {}
    } else {
        let env_lookup_helper = quote! {
            #[expect(
                clippy::disallowed_methods,
                reason = "e2e_test live-mode guard intentionally checks process env for declared live secrets."
            )]
            fn __fabro_e2e_env_var_is_missing(name: &str) -> bool {
                ::std::env::var(name).is_err()
            }
        };
        let guards = env_vars.iter().map(|env_var| {
            let env_name = env_var.value();
            let strict_message = format!("{env_name} not set (FABRO_TEST_MODE=strict)");
            let skip_message = format!("skipping: {env_name} not set");
            quote! {
                if __fabro_e2e_env_var_is_missing(#env_var) {
                    if __mode == ::fabro_test::TestMode::Strict {
                        panic!(#strict_message);
                    }
                    eprintln!(#skip_message);
                    return;
                }
            }
        });

        if has_twin {
            // dual-mode: only check env vars when in live/strict
            quote! {
                #env_lookup_helper

                if __mode.is_live() {
                    #(#guards)*
                }
            }
        } else {
            // live-only: always check env vars (mode guard already skipped twin)
            quote! {
                #env_lookup_helper

                #(#guards)*
            }
        }
    };

    quote! {
        #(#attrs)*
        #test_attr
        #[ignore = #ignore_reason]
        #vis #sig {
            let __mode = ::fabro_test::TestMode::from_env();

            #mode_guard

            #env_guards

            #block
        }
    }
    .into()
}

#[proc_macro_derive(Combine)]
pub fn derive_combine(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    impl_combine(&input)
}

fn impl_combine(ast: &DeriveInput) -> TokenStream {
    let name = &ast.ident;
    let fields = match ast.data {
        syn::Data::Struct(syn::DataStruct {
            fields: syn::Fields::Named(ref fields),
            ..
        }) => &fields.named,
        _ => {
            return syn::Error::new_spanned(
                ast,
                "Combine can only be derived for structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };
    let (impl_generics, ty_generics, where_clause) = ast.generics.split_for_impl();

    let combines = fields.iter().map(|field| {
        let name = &field.ident;
        quote! {
            #name: ::fabro_config::layers::Combine::combine(self.#name, other.#name)
        }
    });

    quote! {
        impl #impl_generics ::fabro_config::layers::Combine for #name #ty_generics #where_clause {
            fn combine(self, other: Self) -> Self {
                Self {
                    #(#combines),*
                }
            }
        }
    }
    .into()
}
