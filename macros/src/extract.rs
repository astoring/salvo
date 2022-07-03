use darling::{FromDeriveInput, FromField, FromMeta};
use proc_macro2::{Ident, TokenStream};
use quote::{quote, format_ident};
use syn::{Attribute, DeriveInput, Error, Generics, Lit, Meta, NestedMeta};

use crate::shared::salvo_crate;

// #[derive(Debug)]
struct Field {
    ident: Option<Ident>,
    // attrs: Vec<Attribute>,
    sources: Vec<RawSource>,
    aliases: Vec<String>,
    rename: Option<String>,
}
#[derive(FromMeta, Debug)]
struct RawSource {
    from: String,
    #[darling(default)]
    format: String,
}

impl FromField for Field {
    fn from_field(field: &syn::Field) -> darling::Result<Self> {
        let ident = field.ident.clone();
        let attrs = field.attrs.clone();
        let sources = parse_sources(&attrs, "source")?;
        Ok(Self {
            ident,
            // attrs,
            sources,
            aliases: parse_aliases(&field.attrs)?,
            rename: parse_rename(&field.attrs)?,
        })
    }
}

struct ExtractibleArgs {
    ident: Ident,
    generics: Generics,
    fields: Vec<Field>,

    internal: bool,

    default_sources: Vec<RawSource>,
    rename_all: Option<String>,
}

impl FromDeriveInput for ExtractibleArgs {
    fn from_derive_input(input: &DeriveInput) -> darling::Result<Self> {
        let ident = input.ident.clone();
        let generics = input.generics.clone();
        let attrs = input.attrs.clone();
        let default_sources = parse_sources(&attrs, "default_source")?;
        let data = match &input.data {
            syn::Data::Struct(data) => data,
            _ => {
                return Err(Error::new_spanned(ident, "Extractible can only be applied to an struct.").into());
            }
        };
        let mut fields = Vec::with_capacity(data.fields.len());
        for field in data.fields.iter() {
            fields.push(Field::from_field(field)?);
        }
        let mut internal = false;
        for attr in &attrs {
            if attr.path.is_ident("extract") {
                if let Meta::List(list) = attr.parse_meta()? {
                    for meta in list.nested.iter() {
                        if matches!(meta, NestedMeta::Meta(Meta::Path(item)) if item.is_ident("internal")) {
                            internal = true;
                        }
                    }
                }
                if internal {
                    break;
                }
            }
        }
        Ok(Self {
            ident,
            generics,
            fields,
            internal,
            default_sources,
            rename_all: parse_rename_rule(&input.attrs)?,
        })
    }
}

pub(crate) fn generate(args: DeriveInput) -> Result<TokenStream, Error> {
    let mut args: ExtractibleArgs = ExtractibleArgs::from_derive_input(&args)?;
    let salvo = salvo_crate(args.internal);
    let (impl_generics, ty_generics, where_clause) = args.generics.split_for_impl();

    let ident = &args.ident;
    let mut default_sources = Vec::new();
    let mut fields = Vec::new();

    for source in &args.default_sources {
        let from = &source.from;
        let format = &source.format;
        default_sources.push(quote! {
            metadata = metadata.add_default_source(#salvo::extract::metadata::Source::new(#from.parse().unwrap(), #format.parse().unwrap()));
        });
    }
    let rename_all = args.rename_all.map(|rename| {
        quote! {
            metadata = metadata.rename_all(#rename);
        }
    });

    for field in &mut args.fields {
        let field_ident = field
            .ident
            .as_ref()
            .ok_or_else(|| Error::new_spanned(&ident, "All fields must be named."))?
            .to_string();
        // let field_ty = field.ty.to_string();

        let mut sources = Vec::with_capacity(field.sources.len());
        for source in &field.sources {
            let from = &source.from;
            let format = &source.format;
            sources.push(quote! {
                field = field.add_source(#salvo::extract::metadata::Source::new(#from.parse().unwrap(), #format.parse().unwrap()));
            });
        }
        let aliases = field.aliases.iter().map(|alias| {
            quote! {
                field = field.add_alias(#alias);
            }
        });
        let rename = field.rename.as_ref().map(|rename| {
            quote! {
                field = field.rename(#rename);
            }
        });
        for source in &field.sources {
            let from = &source.from;
            let format = &source.format;
            sources.push(quote! {
                field = field.add_source(#salvo::extract::metadata::Source::new(#from.parse().unwrap(), #format.parse().unwrap()));
            });
        }
        fields.push(quote! {
            let mut field = #salvo::extract::metadata::Field::new(#field_ident, "struct".parse().unwrap());
            #(#sources)*
            #(#aliases)*
            #rename
            metadata = metadata.add_field(field);
        });
    }

    let sv = format_ident!("__salvo_extract_{}", ident);
    let mt = ident.to_string();
    let imp_code = if args.generics.lifetimes().next().is_none() {
        let de_life_def = syn::parse_str("'de").unwrap();
        let mut generics = args.generics.clone();
        generics.params.insert(0, de_life_def);
        let impl_generics_de = generics.split_for_impl().0;
        quote! {
            impl #impl_generics_de #salvo::extract::Extractible<'de> for #ident #ty_generics #where_clause {
                fn metadata() ->  &'static #salvo::extract::Metadata {
                    &*#sv
                }
            }
        }
    } else {
        quote! {
            impl #impl_generics #salvo::extract::Extractible #impl_generics for #ident #ty_generics #where_clause {
                fn metadata() ->  &'static #salvo::extract::Metadata {
                    &*#sv
                }
            }
        }
    };
    let code = quote! {
        #[allow(non_upper_case_globals)]
        static #sv: #salvo::__private::once_cell::sync::Lazy<#salvo::extract::Metadata> = #salvo::__private::once_cell::sync::Lazy::new(||{
            let mut metadata = #salvo::extract::Metadata::new(#mt, #salvo::extract::metadata::DataKind::Struct);
            #(
                #default_sources
            )*
            #rename_all
            #(
                #fields
            )*
            metadata
        });
        #imp_code
    };

    Ok(code)
}

fn parse_rename(attrs: &[syn::Attribute]) -> darling::Result<Option<String>> {
    for attr in attrs {
        if attr.path.is_ident("extract") {
            if let Meta::List(list) = attr.parse_meta()? {
                for meta in list.nested.iter() {
                    if let NestedMeta::Meta(Meta::NameValue(item)) = meta {
                        if item.path.is_ident("rename") {
                            if let Lit::Str(lit) = &item.lit {
                                return Ok(Some(lit.value()));
                            } else {
                                return Err(darling::Error::custom(format!("invalid rename: {:?}", item)));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

fn parse_rename_rule(attrs: &[syn::Attribute]) -> darling::Result<Option<String>> {
    for attr in attrs {
        if attr.path.is_ident("extract") {
            if let Meta::List(list) = attr.parse_meta()? {
                for meta in list.nested.iter() {
                    if let NestedMeta::Meta(Meta::NameValue(item)) = meta {
                        if item.path.is_ident("rename_all") {
                            if let Lit::Str(lit) = &item.lit {
                                return Ok(Some(lit.value()));
                            } else {
                                return Err(darling::Error::custom(format!("invalid alias: {:?}", item)));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

fn parse_aliases(attrs: &[syn::Attribute]) -> darling::Result<Vec<String>> {
    let mut aliases = Vec::new();
    for attr in attrs {
        if attr.path.is_ident("extract") {
            if let Meta::List(list) = attr.parse_meta()? {
                for meta in list.nested.iter() {
                    if let NestedMeta::Meta(Meta::NameValue(item)) = meta {
                        if item.path.is_ident("alias") {
                            if let Lit::Str(lit) = &item.lit {
                                aliases.push(lit.value());
                            } else {
                                return Err(darling::Error::custom(format!("invalid alias: {:?}", item)));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(aliases)
}

fn parse_sources(attrs: &[Attribute], key: &str) -> darling::Result<Vec<RawSource>> {
    let mut sources = Vec::with_capacity(4);
    for attr in attrs {
        if attr.path.is_ident("extract") {
            if let Meta::List(list) = attr.parse_meta()? {
                for meta in list.nested.iter() {
                    if matches!(meta, NestedMeta::Meta(Meta::List(item)) if item.path.is_ident(key)) {
                        let mut source: RawSource = FromMeta::from_nested_meta(meta)?;
                        if source.format.is_empty() {
                            if source.format == "request" {
                                source.format = "request".to_string();
                            } else {
                                source.format = "multimap".to_string();
                            }
                        }
                        if !["request", "param", "query", "header", "body"].contains(&source.from.as_str()) {
                            return Err(darling::Error::custom(format!(
                                "source from is invalid: {}",
                                source.from
                            )));
                        }
                        if !["multimap", "json", "request"].contains(&source.format.as_str()) {
                            return Err(darling::Error::custom(format!(
                                "source format is invalid: {}",
                                source.format
                            )));
                        }
                        if source.from == "request" && source.format != "request" {
                            return Err(darling::Error::custom(
                                "source format must be `request` for `request` sources",
                            ));
                        }
                        sources.push(source);
                    }
                }
            }
        }
    }
    Ok(sources)
}