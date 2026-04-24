use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DataStruct, DeriveInput, ExprLit, Field, Fields, GenericArgument, Lit, Meta,
    PathArguments, Token, Type,
};

pub(crate) fn derive_impl(input: DeriveInput) -> syn::Result<TokenStream> {
    let DeriveInput {
        ident,
        data,
        attrs,
        generics,
        ..
    } = input;

    let Data::Struct(DataStruct {
        fields: Fields::Named(fields),
        ..
    }) = data
    else {
        return Err(syn::Error::new(
            ident.span(),
            "OptionsMetadata can only be derived for structs with named fields",
        ));
    };

    let mut records = Vec::new();
    for field in &fields.named {
        if let Some(attr) = field
            .attrs
            .iter()
            .find(|attr| attr.path().is_ident("option"))
        {
            records.push(handle_option(field, attr)?);
        } else if field
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("option_group"))
        {
            records.push(handle_option_group(field)?);
        } else if has_serde_flatten(field)? {
            let ty = &field.ty;
            records.push(quote_spanned!(ty.span() => <#ty as ::fabro_options_metadata::OptionsMetadata>::record(visit)));
        }
    }

    let documentation = quote_option_str(doc_string(&attrs)?);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::fabro_options_metadata::OptionsMetadata for #ident #ty_generics #where_clause {
            fn record(visit: &mut dyn ::fabro_options_metadata::Visit) {
                #(#records;)*
            }

            fn documentation() -> Option<&'static str> {
                #documentation
            }
        }
    })
}

fn handle_option_group(field: &Field) -> syn::Result<TokenStream> {
    let ident = field_ident(field)?;
    let name = option_name(field)?;
    let ty = get_inner_type_if_option(&field.ty).unwrap_or(&field.ty);

    Ok(quote_spanned!(
        ident.span() => visit.record_set(#name, ::fabro_options_metadata::OptionSet::of::<#ty>())
    ))
}

fn handle_option(field: &Field, attr: &Attribute) -> syn::Result<TokenStream> {
    let ident = field_ident(field)?;
    let attrs = parse_option_attributes(attr)?;
    let name = attrs
        .name
        .clone()
        .or(option_long_name(field)?)
        .unwrap_or_else(|| ident.to_string().replace('_', "-"));
    let doc = quote_option_str(doc_string(&field.attrs)?);
    let default = quote_option_str(attrs.default);
    let value_type = quote_option_str(attrs.value_type);
    let scope = quote_option_str(attrs.scope);
    let example = quote_option_str(attrs.example);
    let added_in = quote_option_str(attrs.added_in);
    let deprecated = deprecated_metadata(field)?;
    let possible_values = if attrs.possible_values.unwrap_or(false) || has_value_enum_arg(field)? {
        let ty = get_inner_type_if_option(&field.ty).unwrap_or(&field.ty);
        quote! {
            Some(
                <#ty as ::clap::ValueEnum>::value_variants()
                    .iter()
                    .filter_map(::clap::ValueEnum::to_possible_value)
                    .map(|value| ::fabro_options_metadata::PossibleValue {
                        name: value.get_name().to_string(),
                        help: value.get_help().map(ToString::to_string),
                    })
                    .collect()
            )
        }
    } else {
        quote!(None)
    };

    Ok(quote_spanned!(
        ident.span() => visit.record_field(#name, ::fabro_options_metadata::OptionField {
            doc: #doc,
            default: #default,
            value_type: #value_type,
            scope: #scope,
            example: #example,
            deprecated: #deprecated,
            possible_values: #possible_values,
            added_in: #added_in,
        })
    ))
}

fn field_ident(field: &Field) -> syn::Result<&syn::Ident> {
    field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new(field.span(), "expected named field"))
}

fn option_name(field: &Field) -> syn::Result<String> {
    let ident = field_ident(field)?;
    Ok(ident.to_string().replace('_', "-"))
}

fn doc_string(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    let docs = attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .map(parse_doc)
        .collect::<syn::Result<Vec<_>>>()?;
    let doc = docs
        .into_iter()
        .map(|line| line.trim().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string();

    if doc.is_empty() {
        Ok(None)
    } else {
        Ok(Some(doc))
    }
}

fn parse_doc(attr: &Attribute) -> syn::Result<String> {
    match &attr.meta {
        Meta::NameValue(name_value) => match &name_value.value {
            syn::Expr::Lit(ExprLit {
                lit: Lit::Str(lit), ..
            }) => Ok(lit.value()),
            value => Err(syn::Error::new(value.span(), "expected doc string literal")),
        },
        meta => Err(syn::Error::new(meta.span(), "expected doc attribute")),
    }
}

#[derive(Default)]
struct FieldAttributes {
    name:            Option<String>,
    default:         Option<String>,
    value_type:      Option<String>,
    scope:           Option<String>,
    example:         Option<String>,
    possible_values: Option<bool>,
    added_in:        Option<String>,
}

fn parse_option_attributes(attr: &Attribute) -> syn::Result<FieldAttributes> {
    let mut attrs = FieldAttributes::default();

    match &attr.meta {
        Meta::Path(_) => return Ok(attrs),
        Meta::List(_) => {}
        meta @ Meta::NameValue(_) => {
            return Err(syn::Error::new(
                meta.span(),
                "expected `#[option]` or `#[option(...)]`",
            ));
        }
    }

    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            attrs.name = Some(string_literal(&meta, "name", "option")?.value());
        } else if meta.path.is_ident("default") {
            attrs.default = Some(string_literal(&meta, "default", "option")?.value());
        } else if meta.path.is_ident("value_type") {
            attrs.value_type = Some(string_literal(&meta, "value_type", "option")?.value());
        } else if meta.path.is_ident("scope") {
            attrs.scope = Some(string_literal(&meta, "scope", "option")?.value());
        } else if meta.path.is_ident("example") {
            attrs.example = Some(string_literal(&meta, "example", "option")?.value());
        } else if meta.path.is_ident("possible_values") {
            attrs.possible_values = Some(bool_literal(&meta, "possible_values", "option")?);
        } else if meta.path.is_ident("added_in") {
            attrs.added_in = Some(string_literal(&meta, "added_in", "option")?.value());
        } else {
            return Err(syn::Error::new(
                meta.path.span(),
                "unsupported `option` metadata key",
            ));
        }

        Ok(())
    })?;

    Ok(attrs)
}

fn deprecated_metadata(field: &Field) -> syn::Result<TokenStream> {
    let Some(attr) = field
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("deprecated"))
    else {
        return Ok(quote!(None));
    };

    let mut since = None;
    let mut message = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("since") {
            since = Some(string_literal(&meta, "since", "deprecated")?.value());
        } else if meta.path.is_ident("note") {
            message = Some(string_literal(&meta, "note", "deprecated")?.value());
        } else {
            return Err(syn::Error::new(
                meta.path.span(),
                "unsupported `deprecated` metadata key",
            ));
        }

        Ok(())
    })?;

    let since = quote_option_str(since);
    let message = quote_option_str(message);
    Ok(quote!(Some(::fabro_options_metadata::Deprecated {
        since: #since,
        message: #message,
    })))
}

fn option_long_name(field: &Field) -> syn::Result<Option<String>> {
    let mut long = None;
    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("arg") || attr.path().is_ident("clap"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("long") {
                if meta.input.peek(syn::Token![=]) {
                    long = Some(string_literal(&meta, "long", "arg")?.value());
                } else {
                    long = Some(option_name(field)?);
                }
            }
            Ok(())
        })?;
    }

    Ok(long)
}

fn has_value_enum_arg(field: &Field) -> syn::Result<bool> {
    let mut has_value_enum = false;
    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("arg") || attr.path().is_ident("clap"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("value_enum") {
                has_value_enum = true;
            }
            Ok(())
        })?;
    }

    Ok(has_value_enum)
}

fn has_serde_flatten(field: &Field) -> syn::Result<bool> {
    let mut flatten = false;
    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("flatten") {
                flatten = true;
            }
            if meta.input.peek(Token![=]) {
                let _ = meta.value()?.parse::<syn::Expr>()?;
            }
            Ok(())
        })?;
    }
    Ok(flatten)
}

fn get_inner_type_if_option(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.path.segments.len() != 1 || type_path.path.segments[0].ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &type_path.path.segments[0].arguments else {
        return None;
    };
    if args.args.len() != 1 {
        return None;
    }
    let GenericArgument::Type(inner) = &args.args[0] else {
        return None;
    };
    Some(inner)
}

fn string_literal(
    meta: &ParseNestedMeta<'_>,
    meta_name: &str,
    attribute_name: &str,
) -> syn::Result<syn::LitStr> {
    let expr: syn::Expr = meta.value()?.parse()?;
    let mut value = &expr;
    while let syn::Expr::Group(group) = value {
        value = &group.expr;
    }

    if let syn::Expr::Lit(ExprLit {
        lit: Lit::Str(lit), ..
    }) = value
    {
        Ok(lit.clone())
    } else {
        Err(syn::Error::new(
            expr.span(),
            format!("expected {attribute_name} attribute to be a string: `{meta_name} = \"...\"`"),
        ))
    }
}

fn bool_literal(
    meta: &ParseNestedMeta<'_>,
    meta_name: &str,
    attribute_name: &str,
) -> syn::Result<bool> {
    let expr: syn::Expr = meta.value()?.parse()?;
    let mut value = &expr;
    while let syn::Expr::Group(group) = value {
        value = &group.expr;
    }

    if let syn::Expr::Lit(ExprLit {
        lit: Lit::Bool(lit),
        ..
    }) = value
    {
        Ok(lit.value)
    } else {
        Err(syn::Error::new(
            expr.span(),
            format!("expected {attribute_name} attribute to be a boolean: `{meta_name} = true`"),
        ))
    }
}

fn quote_option_str(value: Option<String>) -> TokenStream {
    if let Some(value) = value {
        quote!(Some(#value))
    } else {
        quote!(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_option_attribute_reports_clear_error() {
        let input: DeriveInput = syn::parse_quote! {
            struct Args {
                #[option(unknown = "value")]
                field: bool,
            }
        };

        let error = derive_impl(input).expect_err("unknown option key should fail");
        assert!(
            error
                .to_string()
                .contains("unsupported `option` metadata key")
        );
    }
}
