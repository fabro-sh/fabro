use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

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
