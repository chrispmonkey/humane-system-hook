//! Derives for the settings descriptor (`api::settings_schema`). A field's key
//! and value wiring come from the config type itself; only the UI semantics a
//! type can't express — label, whether it's a secret/enum/multiline, and
//! conditional visibility — are given as `#[setting(...)]` attributes.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr, Type};

/// The descriptor module path the generated code refers to.
fn rt() -> proc_macro2::TokenStream {
    quote!(crate::api::settings_schema)
}

/// `#[derive(SettingsFields)]` on a config sub-struct: emits one descriptor
/// `Field` per `#[setting(...)]`-annotated field, keyed `<prefix>.<field>`.
#[proc_macro_derive(SettingsFields, attributes(setting))]
pub fn derive_settings_fields(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ty = &input.ident;
    let rt = rt();

    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(ty, "SettingsFields only supports structs")
            .to_compile_error()
            .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(ty, "SettingsFields needs named fields")
            .to_compile_error()
            .into();
    };

    let mut pushes = Vec::new();
    for field in &fields.named {
        let Some(attr) = FieldAttr::parse(&field.attrs) else {
            continue; // no #[setting] → not exposed
        };
        let name = field.ident.as_ref().unwrap();
        let kind = match kind_expr(&attr, name, &field.ty, &rt) {
            Ok(k) => k,
            Err(e) => return e.to_compile_error().into(),
        };
        let label = attr.label;
        let visible = match attr.visible_when {
            Some((k, v)) => quote!(::core::option::Option::Some(
                ::std::collections::BTreeMap::from([(#k, #v)])
            )),
            None => quote!(::core::option::Option::None),
        };
        pushes.push(quote! {
            fields.push(#rt::Field {
                key: ::std::format!("{}.{}", prefix, ::core::stringify!(#name)),
                label: #label,
                kind: #kind,
                visible_when: #visible,
            });
        });
    }

    quote! {
        impl #rt::SettingsFields for #ty {
            fn settings_fields(&self, prefix: &str) -> ::std::vec::Vec<#rt::Field> {
                let mut fields = ::std::vec::Vec::new();
                #(#pushes)*
                fields
            }
        }
    }
    .into()
}

/// Build the `FieldKind` expression for one field from its attribute + type.
fn kind_expr(
    attr: &FieldAttr,
    name: &syn::Ident,
    ty: &Type,
    rt: &proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    // Value for a string/text field: an explicit getter, else the field (an
    // Option collapses to its value or "").
    let string_value = || {
        if let Some(getter) = &attr.get {
            let g = syn::Ident::new(getter, name.span());
            quote!(self.#g())
        } else if is_option(ty) {
            quote!(self.#name.clone().unwrap_or_default())
        } else {
            quote!(self.#name.clone())
        }
    };

    Ok(match attr.kind.as_deref() {
        Some("secret") => {
            if !is_option(ty) {
                return Err(syn::Error::new_spanned(ty, "a secret field must be Option<_>"));
            }
            quote!(#rt::FieldKind::Secret { is_set: self.#name.is_some() })
        }
        Some("enum") => quote!(#rt::FieldKind::Enum {
            value: #rt::SettingsOptions::settings_value(&self.#name),
            options: <#ty as #rt::SettingsOptions>::settings_options(),
        }),
        Some("text") => {
            let v = string_value();
            quote!(#rt::FieldKind::Text { value: #v })
        }
        Some(other) => {
            return Err(syn::Error::new_spanned(
                name,
                format!("unknown setting kind `{other}` (expected secret, enum, or text)"),
            ))
        }
        None if is_bool(ty) => quote!(#rt::FieldKind::Bool { value: self.#name }),
        None => {
            let v = string_value();
            quote!(#rt::FieldKind::String { value: #v })
        }
    })
}

/// `#[derive(SettingsOptions)]` on a fieldless enum: emits its dropdown options
/// (value = the enum's own `as_str`, label from `#[setting(label = "…")]`) and
/// the selected value, so the descriptor needs no external enum crate.
#[proc_macro_derive(SettingsOptions, attributes(setting))]
pub fn derive_settings_options(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ty = &input.ident;
    let rt = rt();

    let Data::Enum(data) = &input.data else {
        return syn::Error::new_spanned(ty, "SettingsOptions only supports enums")
            .to_compile_error()
            .into();
    };

    let mut options = Vec::new();
    for variant in &data.variants {
        let ident = &variant.ident;
        let Some(attr) = FieldAttr::parse(&variant.attrs) else {
            return syn::Error::new_spanned(ident, "each variant needs #[setting(label = \"…\")]")
                .to_compile_error()
                .into();
        };
        let label = attr.label;
        options.push(quote! {
            #rt::EnumOption { value: Self::#ident.as_str(), label: #label }
        });
    }

    quote! {
        impl #rt::SettingsOptions for #ty {
            fn settings_options() -> ::std::vec::Vec<#rt::EnumOption> {
                ::std::vec![#(#options),*]
            }
            fn settings_value(&self) -> &'static str {
                self.as_str()
            }
        }
    }
    .into()
}

/// Parsed `#[setting(...)]` — the UI facts a type can't carry on its own.
struct FieldAttr {
    label: LitStr,
    kind: Option<String>,
    get: Option<String>,
    visible_when: Option<(LitStr, LitStr)>,
}

impl FieldAttr {
    /// Returns `None` when the item carries no `#[setting]` (i.e. not exposed).
    fn parse(attrs: &[syn::Attribute]) -> Option<FieldAttr> {
        let attr = attrs.iter().find(|a| a.path().is_ident("setting"))?;
        let mut label = None;
        let mut kind = None;
        let mut get = None;
        let mut visible_when = None;

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("label") {
                label = Some(meta.value()?.parse::<LitStr>()?);
            } else if meta.path.is_ident("get") {
                get = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("secret") {
                kind = Some("secret".to_string());
            } else if meta.path.is_ident("enumerated") {
                kind = Some("enum".to_string());
            } else if meta.path.is_ident("text") {
                kind = Some("text".to_string());
            } else if meta.path.is_ident("visible_when") {
                let mut when = None;
                let mut equals = None;
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("field") {
                        when = Some(m.value()?.parse::<LitStr>()?);
                    } else if m.path.is_ident("equals") {
                        equals = Some(m.value()?.parse::<LitStr>()?);
                    }
                    Ok(())
                })?;
                match (when, equals) {
                    (Some(w), Some(e)) => visible_when = Some((w, e)),
                    _ => return Err(meta.error("visible_when needs field = \"…\", equals = \"…\"")),
                }
            } else {
                return Err(meta.error("unknown setting attribute"));
            }
            Ok(())
        })
        .expect("invalid #[setting(...)]");

        Some(FieldAttr {
            label: label.expect("#[setting] requires label = \"…\""),
            kind,
            get,
            visible_when,
        })
    }
}

fn is_option(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

fn is_bool(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.is_ident("bool"))
}
