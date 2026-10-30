//! `#[derive(Options)]` on a config struct as written generates the rest of
//! its option handling from one declaration: the resolved struct, the
//! built-in layer, the per-option merge and provenance walk, and `resolve`.
//! The trait it implements, and the helpers the generated code calls, live in
//! Shoal's `config::options` module.
//!
//! The struct names its resolved form with `#[options(resolved = Name)]`, or
//! with `#[options(resolved = Self)]` when every option resolves to the value
//! as written.
//! Each field takes at most one kind and any of the modifiers:
//!
//! - no kind: an `Option<T>` a layer sets or omits, still `Option<T>` when
//!   resolved
//! - `section`: a nested struct that derives `Options`
//! - `named`: a map whose entries replace the same-named entry below them whole
//! - `list`: a list an empty value leaves to the layer below
//! - `template`: an `Option<String>` a repository value is appended to
//! - `repository_only`: rejected in the global file
//! - `default = expr`: the built-in value; an `Option<T>` resolves to `T`
//! - `resolve = path` or `resolve = path -> Type`: converts the merged value,
//!   given with the merged struct it belongs to, into the resolved one
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::{
    Attribute, Data, DeriveInput, Error, Expr, Fields, GenericArgument, Ident, LitStr, Path,
    PathArguments, Result, Token, Type, Visibility, parse_macro_input,
};

#[proc_macro_derive(Options, attributes(options, option))]
pub fn derive_options(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> Result<Tokens> {
    let declaration = Declaration::parse(input)?;
    let definition = declaration.resolved_struct();
    let implementation = declaration.implementation();
    Ok(quote!(#definition #implementation))
}

/// The written struct with its options, checked.
struct Declaration {
    written: Ident,
    visibility: Visibility,
    /// The resolved struct's name; `None` when options resolve as written.
    resolved: Option<Ident>,
    fields: Vec<Field>,
}

impl Declaration {
    fn parse(input: &DeriveInput) -> Result<Self> {
        let resolved = resolved_name(input)?;
        let Data::Struct(data) = &input.data else {
            return Err(Error::new_spanned(input, "Options derives only on structs"));
        };
        let Fields::Named(named) = &data.fields else {
            return Err(Error::new_spanned(input, "Options needs named fields"));
        };
        let fields = named
            .named
            .iter()
            .map(Field::parse)
            .collect::<Result<Vec<_>>>()?;
        if resolved.is_none()
            && let Some(field) = fields.iter().find(|field| field.changes_type())
        {
            return Err(Error::new_spanned(
                &field.ident,
                "this option resolves to another type; name the resolved struct",
            ));
        }
        Ok(Self {
            written: input.ident.clone(),
            visibility: input.vis.clone(),
            resolved,
            fields,
        })
    }

    fn resolved_type(&self) -> Tokens {
        match &self.resolved {
            Some(name) => quote!(#name),
            None => quote!(Self),
        }
    }

    /// The resolved struct, with each field's docs and resolved type.
    fn resolved_struct(&self) -> Tokens {
        let Some(name) = &self.resolved else {
            return quote!();
        };
        let fields = self.fields.iter().map(Field::resolved_declaration);
        let visibility = &self.visibility;
        let doc = format!("[`{}`] after every layer.", self.written);
        quote! {
            #[doc = #doc]
            #[derive(Debug, Clone, ::serde::Serialize)]
            #visibility struct #name { #(#fields),* }
        }
    }

    /// The `Options` implementation, one method per field walk.
    fn implementation(&self) -> Tokens {
        let written = &self.written;
        let resolved = self.resolved_type();
        let fields = &self.fields;
        let built_in: Vec<_> = fields.iter().filter_map(Field::built_in).collect();
        let rest =
            (built_in.len() < fields.len()).then(|| quote!(..::core::default::Default::default()));
        let merges = fields.iter().map(Field::merge);
        let bindings = fields.iter().map(|field| &field.ident);
        let conversions = fields.iter().filter_map(Field::conversion);
        let values = fields.iter().map(Field::resolved_value);
        let entries = fields.iter().map(Field::entries);
        let repository_only = fields.iter().filter_map(Field::repository_only);
        quote! {
            #[allow(unused_variables)]
            impl crate::config::options::Options for #written {
                type Resolved = #resolved;

                fn built_in() -> Self {
                    Self {
                        #(#built_in,)*
                        #rest
                    }
                }

                fn merge(&mut self, top: Self, merge: crate::config::options::Merge) {
                    let Self { #(#bindings),* } = top;
                    #(#merges)*
                }

                fn resolve(self, prefix: &str) -> ::anyhow::Result<#resolved> {
                    #(#conversions)*
                    Ok(#resolved { #(#values),* })
                }

                fn entries(
                    prefix: &str,
                    sources: &[(&Self, crate::config::repo::ConfigLayer)],
                    entries: &mut ::std::vec::Vec<crate::config::options::Entry>,
                ) -> ::anyhow::Result<()> {
                    #(#entries)*
                    Ok(())
                }

                fn repository_only(
                    prefix: &str,
                    keys: &mut ::std::vec::Vec<::std::string::String>,
                ) {
                    #(#repository_only)*
                }
            }
        }
    }
}

/// The resolved struct's name; `None` for `Self`.
fn resolved_name(input: &DeriveInput) -> Result<Option<Ident>> {
    let mut resolved = None;
    for attribute in input.attrs.iter().filter(|a| a.path().is_ident("options")) {
        attribute.parse_nested_meta(|meta| {
            if !meta.path.is_ident("resolved") {
                return Err(meta.error("expected `resolved = Name`"));
            }
            let value = meta.value()?;
            resolved = Some(if value.peek(Token![Self]) {
                value.parse::<Token![Self]>()?;
                None
            } else {
                Some(value.parse::<Ident>()?)
            });
            Ok(())
        })?;
    }
    resolved.ok_or_else(|| Error::new_spanned(&input.ident, "add #[options(resolved = Name)]"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Scalar,
    Section,
    Named,
    List,
    Template,
}

/// The field's TOML key, which provenance reports.
enum Key {
    Named(String),
    Renamed(String),
    /// Its entries sit in the parent's table.
    Flatten,
}

struct Field {
    ident: Ident,
    visibility: Visibility,
    ty: Type,
    docs: Vec<Attribute>,
    key: Key,
    kind: Kind,
    default: Option<Expr>,
    repository_only: bool,
    resolve: Option<(Path, Option<Type>)>,
}

impl Field {
    fn parse(field: &syn::Field) -> Result<Self> {
        let ident = field
            .ident
            .clone()
            .ok_or_else(|| Error::new_spanned(field, "Options needs named fields"))?;
        let mut parsed = Self {
            key: serde_key(&field.attrs, &ident)?,
            ident,
            visibility: field.vis.clone(),
            ty: field.ty.clone(),
            docs: field
                .attrs
                .iter()
                .filter(|a| a.path().is_ident("doc"))
                .cloned()
                .collect(),
            kind: Kind::Scalar,
            default: None,
            repository_only: false,
            resolve: None,
        };
        for attribute in field.attrs.iter().filter(|a| a.path().is_ident("option")) {
            attribute.parse_nested_meta(|meta| {
                let kind = [
                    ("section", Kind::Section),
                    ("named", Kind::Named),
                    ("list", Kind::List),
                    ("template", Kind::Template),
                ]
                .into_iter()
                .find(|(name, _)| meta.path.is_ident(name));
                if let Some((_, kind)) = kind {
                    if parsed.kind != Kind::Scalar {
                        return Err(meta.error("an option takes one kind"));
                    }
                    parsed.kind = kind;
                } else if meta.path.is_ident("default") {
                    parsed.default = Some(meta.value()?.parse()?);
                } else if meta.path.is_ident("repository_only") {
                    parsed.repository_only = true;
                } else if meta.path.is_ident("resolve") {
                    let path = meta.value()?.parse::<Path>()?;
                    let ty = if meta.input.peek(Token![->]) {
                        meta.input.parse::<Token![->]>()?;
                        Some(meta.input.parse::<Type>()?)
                    } else {
                        None
                    };
                    parsed.resolve = Some((path, ty));
                } else {
                    return Err(meta.error(
                        "expected section, named, list, template, default, \
                         repository_only or resolve",
                    ));
                }
                Ok(())
            })?;
        }
        if parsed.kind == Kind::Section && parsed.default.is_some() {
            return Err(Error::new_spanned(
                &parsed.ident,
                "a section's defaults belong on its own fields",
            ));
        }
        if parsed.unwraps_default() {
            option_inner(&parsed.ty)?;
        }
        Ok(parsed)
    }

    /// The field as the resolved struct declares it.
    fn resolved_declaration(&self) -> Tokens {
        let Self {
            ident, visibility, ..
        } = self;
        let docs = &self.docs;
        let ty = self.resolved_type();
        let serde = match &self.key {
            Key::Flatten => quote!(#[serde(flatten)]),
            Key::Renamed(name) => quote!(#[serde(rename = #name)]),
            Key::Named(_) => quote!(),
        };
        quote!(#(#docs)* #serde #visibility #ident: #ty)
    }

    /// Whether the resolved value has another type than the written one.
    fn changes_type(&self) -> bool {
        self.kind == Kind::Section
            || self.unwraps_default()
            || matches!(self.resolve, Some((_, Some(_))))
    }

    /// Whether a default makes the resolved value the `Option`'s content.
    fn unwraps_default(&self) -> bool {
        self.default.is_some() && matches!(self.kind, Kind::Scalar | Kind::Template)
    }

    fn resolved_type(&self) -> Tokens {
        if let Some((_, Some(ty))) = &self.resolve {
            return quote!(#ty);
        }
        self.kind_type()
    }

    /// The resolved type before any `resolve` conversion.
    fn kind_type(&self) -> Tokens {
        let ty = &self.ty;
        if self.kind == Kind::Section {
            quote!(<#ty as crate::config::options::Options>::Resolved)
        } else if self.unwraps_default() {
            let inner = option_inner(ty).expect("checked while parsing");
            quote!(#inner)
        } else {
            quote!(#ty)
        }
    }

    fn key(&self) -> Tokens {
        match &self.key {
            Key::Named(name) | Key::Renamed(name) => {
                quote!(crate::config::options::key(prefix, #name))
            }
            Key::Flatten => quote!(prefix.to_owned()),
        }
    }

    fn built_in(&self) -> Option<Tokens> {
        let ident = &self.ident;
        let ty = &self.ty;
        if self.kind == Kind::Section {
            return Some(quote!(#ident: <#ty as crate::config::options::Options>::built_in()));
        }
        let default = self.default.as_ref()?;
        Some(if self.unwraps_default() {
            quote!(#ident: ::core::option::Option::Some(#default))
        } else {
            quote!(#ident: #default)
        })
    }

    fn merge(&self) -> Tokens {
        let ident = &self.ident;
        match self.kind {
            Kind::Scalar => quote!(crate::config::options::scalar(&mut self.#ident, #ident);),
            Kind::Section => quote! {
                crate::config::options::Options::merge(&mut self.#ident, #ident, merge);
            },
            Kind::Named => quote!(self.#ident.extend(#ident);),
            Kind::List => quote!(crate::config::options::list(&mut self.#ident, #ident);),
            Kind::Template => {
                quote!(crate::config::options::template(&mut self.#ident, #ident, merge);)
            }
        }
    }

    /// The merged value as its kind resolves it, from `source`.
    fn kind_value(&self, source: Tokens) -> Tokens {
        let key = self.key();
        match self.kind {
            Kind::Section => quote!(crate::config::options::Options::resolve(#source, &#key)?),
            _ if self.unwraps_default() => {
                quote!(crate::config::options::required(#source, &#key)?)
            }
            _ => source,
        }
    }

    /// Binds the converted value of a field with `resolve`, while `self` is
    /// still whole for the conversion to read.
    fn conversion(&self) -> Option<Tokens> {
        let (path, _) = self.resolve.as_ref()?;
        let ident = &self.ident;
        let value = self.kind_value(quote!(::core::clone::Clone::clone(&self.#ident)));
        let binding = converted(ident);
        Some(quote!(let #binding = #path(#value, &self)?;))
    }

    fn resolved_value(&self) -> Tokens {
        let ident = &self.ident;
        if self.resolve.is_some() {
            let binding = converted(ident);
            return quote!(#ident: #binding);
        }
        let value = self.kind_value(quote!(self.#ident));
        quote!(#ident: #value)
    }

    fn entries(&self) -> Tokens {
        let ident = &self.ident;
        let ty = &self.ty;
        let key = self.key();
        let sources = quote!(&crate::config::options::project(sources, |config| &config.#ident));
        match self.kind {
            Kind::Scalar => {
                quote!(crate::config::options::scalar_entry(#key, #sources, entries)?;)
            }
            Kind::Section => quote! {
                <#ty as crate::config::options::Options>::entries(&#key, #sources, entries)?;
            },
            Kind::Named => {
                quote!(crate::config::options::named_entries(&#key, #sources, entries)?;)
            }
            Kind::List => quote!(crate::config::options::list_entry(#key, #sources, entries)?;),
            Kind::Template => {
                quote!(crate::config::options::template_entry(#key, #sources, entries)?;)
            }
        }
    }

    fn repository_only(&self) -> Option<Tokens> {
        let key = self.key();
        if self.repository_only {
            return Some(quote!(keys.push(#key);));
        }
        let ty = &self.ty;
        (self.kind == Kind::Section).then(
            || quote!(<#ty as crate::config::options::Options>::repository_only(&#key, keys);),
        )
    }
}

fn converted(ident: &Ident) -> Ident {
    format_ident!("__resolved_{ident}")
}

/// The key serde reads the field under.
fn serde_key(attributes: &[Attribute], ident: &Ident) -> Result<Key> {
    let mut key = Key::Named(ident.to_string());
    for attribute in attributes.iter().filter(|a| a.path().is_ident("serde")) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("flatten") {
                key = Key::Flatten;
            } else if meta.path.is_ident("rename") {
                key = Key::Renamed(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.input.peek(Token![=]) {
                meta.value()?.parse::<Expr>()?;
            } else if meta.input.peek(syn::token::Paren) {
                meta.input.parse::<proc_macro2::Group>()?;
            }
            Ok(())
        })?;
    }
    Ok(key)
}

fn option_inner(ty: &Type) -> Result<&Type> {
    let error = || Error::new_spanned(ty, "a default on this kind needs an Option<T> field");
    let Type::Path(path) = ty else {
        return Err(error());
    };
    let segment = path.path.segments.last().ok_or_else(error)?;
    if segment.ident != "Option" {
        return Err(error());
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(error());
    };
    match arguments.args.first() {
        Some(GenericArgument::Type(inner)) => Ok(inner),
        _ => Err(Error::new(
            Span::call_site(),
            "Option needs a type argument",
        )),
    }
}
