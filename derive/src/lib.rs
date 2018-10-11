//! Ethereum (Solidity) derivation for rust contracts (compiled to wasm or otherwise)
#![feature(use_extern_macros)]
#![recursion_limit="256"]
#![deny(unused)]

extern crate proc_macro;
extern crate proc_macro2;

#[macro_use] extern crate syn;
#[macro_use] extern crate quote;
extern crate tiny_keccak;
extern crate byteorder;
extern crate parity_hash;
extern crate serde_json;
#[macro_use] extern crate serde_derive;

mod items;
mod utils;
mod json;

use proc_macro2::{Span};

use items::Item;

/// Extracts arguments from the macro attribute as vector of strings.
/// 
/// # Example
/// 
/// Given the token stream that is represented by the string `"(Foo, Bar)"`
/// then this extracts the strings `Foo` and `Bar` out of it.
fn parse_args_to_vec(args: proc_macro2::TokenStream) -> Vec<String> {
	args.to_string()
		.split(',')
		.map(|w| w.trim_matches(&['(', ')', '"', ' '][..]).to_string())
		.collect()
}

/// Arguments given to the `eth_abi` attribute macro.
struct Args {
	/// The required name of the endpoint.
	endpoint_name: String,
	/// The optional name of the client.
	client_name: Option<String>
}

impl Args {
	/// Returns the given endpoint name.
	pub fn endpoint_name(&self) -> &str {
		&self.endpoint_name
	}

	/// Returns the optional client name.
	pub fn client_name(&self) -> Option<&str> {
		self.client_name.as_ref().map(|s| s.as_str())
	}
}

/// Parses the given token stream as arguments for the `eth_abi` attribute macro.
fn parse_args(args: proc_macro2::TokenStream) -> Args {
	let args = parse_args_to_vec(args);

	assert!(1 <= args.len() && args.len() <= 2,
		"[err01]: Expect one argument for endpoint name and an optional argument for client name.");
	println!("eth_abi::args = {:?}", args);

	let endpoint_name = args.get(0).unwrap().to_owned();
	let client_name = args.get(1).map(|s| s.to_owned());

	Args{ endpoint_name, client_name }
}

/// Derive abi for given trait. Should provide one or two arguments:
/// dispatch structure name and client structure name.
///
/// # Example: Using just one argument
///
/// #[eth_abi(Endpoint)]
/// trait Contract { }
///
/// # Example: Using two arguments
///
/// #[eth_abi(Endpoint2, Client2)]
/// trait Contract2 { }
#[proc_macro_attribute]
pub fn eth_abi(
	args: proc_macro::TokenStream,
	input: proc_macro::TokenStream
)
	-> proc_macro::TokenStream
{
    let args: proc_macro2::TokenStream = args.into();

	let args = parse_args(args);
	let intf = items::Interface::from_item(
		parse_macro_input!(input as syn::Item));

	write_json_abi(&intf);

	let output: proc_macro2::TokenStream = match args.client_name() {
		None => {
			generate_eth_endpoint_wrapper(&intf, args.endpoint_name())
		},
		Some(client_name) => {
			generate_eth_endpoint_and_client_wrapper(&intf, args.endpoint_name(), client_name)
		}
	};

    output.into()
}

fn generate_eth_endpoint_wrapper(
	intf: &items::Interface,
	endpoint_name: &str
)
	-> proc_macro2::TokenStream
{
	// === REFACTORING TARGET ===
	let name_ident_use = syn::Ident::new(intf.name(), Span::call_site());
	let mod_name = format!("pwasm_abi_impl_{}", &intf.name().clone());
	let mod_name_ident = syn::Ident::new(&mod_name, Span::call_site());
	// === REFACTORING TARGET ===

	let endpoint_toks = generate_eth_endpoint(endpoint_name, intf);
	let endpoint_ident = syn::Ident::new(endpoint_name, Span::call_site());
	quote! {
		#intf
		#[allow(non_snake_case)]
		mod #mod_name_ident {
			extern crate pwasm_ethereum;
			extern crate pwasm_abi;
			use pwasm_abi::types::*;
			use super::#name_ident_use;
			#endpoint_toks
		}
		pub use self::#mod_name_ident::#endpoint_ident;
	}
}

fn generate_eth_endpoint_and_client_wrapper(
	intf: &items::Interface,
	endpoint_name: &str,
	client_name: &str
)
	-> proc_macro2::TokenStream
{
	// === REFACTORING TARGET ===
	let name_ident_use = syn::Ident::new(intf.name(), Span::call_site());
	let mod_name = format!("pwasm_abi_impl_{}", &intf.name().clone());
	let mod_name_ident = syn::Ident::new(&mod_name, Span::call_site());
	// === REFACTORING TARGET ===

	let endpoint_toks = generate_eth_endpoint(endpoint_name, &intf);
	let client_toks = generate_eth_client(client_name, &intf);
	let endpoint_name_ident = syn::Ident::new(endpoint_name, Span::call_site());
	let client_name_ident = syn::Ident::new(&client_name, Span::call_site());
	quote! {
		#intf
		#[allow(non_snake_case)]
		mod #mod_name_ident {
			extern crate pwasm_ethereum;
			extern crate pwasm_abi;
			use pwasm_abi::types::*;
			use super::#name_ident_use;
			#endpoint_toks
			#client_toks
		}
		pub use self::#mod_name_ident::#endpoint_name_ident;
		pub use self::#mod_name_ident::#client_name_ident;
	}
}

fn write_json_abi(intf: &items::Interface) {
	use std::fs;
	use std::path::PathBuf;
	use std::env;

	let mut target = PathBuf::from(env::var("CARGO_TARGET_DIR").unwrap_or(".".to_owned()));
	target.push("target");
	target.push("json");
	fs::create_dir_all(&target).expect("failed to create json directory");
	target.push(&format!("{}.json", intf.name()));

	let mut f = fs::File::create(target).expect("failed to write json");
	let abi: json::Abi = intf.into();
	serde_json::to_writer_pretty(&mut f, &abi).expect("failed to write json");
}

fn generate_eth_client(client_name: &str, intf: &items::Interface) -> proc_macro2::TokenStream {
	let client_ctor = intf.constructor().map(
		|signature| utils::produce_signature(
			&signature.name,
			&signature.method_sig,
			quote! {
				#![allow(unused_mut)]
				#![allow(unused_variables)]
				unimplemented!()
			}
		)
	);

	let calls: Vec<proc_macro2::TokenStream> = intf.items().iter().filter_map(|item| {
		match *item {
			Item::Signature(ref signature)  => {
				let hash_literal = syn::Lit::Int(
					syn::LitInt::new(signature.hash as u64, syn::IntSuffix::U32, Span::call_site()));
				let argument_push: Vec<proc_macro2::TokenStream> = utils::iter_signature(&signature.method_sig)
					.map(|(pat, _)| quote! { sink.push(#pat); })
					.collect();
				let argument_count_literal = syn::Lit::Int(
					syn::LitInt::new(argument_push.len() as u64, syn::IntSuffix::Usize, Span::call_site()));

				let result_instance = match signature.method_sig.decl.output {
					syn::ReturnType::Default => quote!{
						let mut result = Vec::new();
					},
					syn::ReturnType::Type(_, _) => quote!{
						let mut result = [0u8; 32];
					},
				};

				let result_pop = match signature.method_sig.decl.output {
					syn::ReturnType::Default => None,
					syn::ReturnType::Type(_, _) => Some(
						quote!{
							let mut stream = pwasm_abi::eth::Stream::new(&result);
							stream.pop().expect("failed decode call output")
						}
					),
				};

				Some(utils::produce_signature(
					&signature.name,
					&signature.method_sig,
					quote!{
						#![allow(unused_mut)]
						#![allow(unused_variables)]
						let mut payload = Vec::with_capacity(4 + #argument_count_literal * 32);
						payload.push((#hash_literal >> 24) as u8);
						payload.push((#hash_literal >> 16) as u8);
						payload.push((#hash_literal >> 8) as u8);
						payload.push(#hash_literal as u8);

						let mut sink = pwasm_abi::eth::Sink::new(#argument_count_literal);
						#(#argument_push)*

						sink.drain_to(&mut payload);

						#result_instance

						pwasm_ethereum::call(self.gas.unwrap_or(200000), &self.address, self.value.clone().unwrap_or(U256::zero()), &payload, &mut result[..])
							.expect("Call failed; todo: allow handling inside contracts");

						#result_pop
					}
				))
			},
			Item::Event(ref event)  => {
				Some(utils::produce_signature(
					&event.name,
					&event.method_sig,
					quote!{
						#![allow(unused_variables)]
						panic!("cannot use event in client interface");
					}
				))
			},
			_ => None,
		}
	}).collect();

	let client_ident = syn::Ident::new(client_name, Span::call_site());
	let name_ident = syn::Ident::new(intf.name(), Span::call_site());

	quote! {
		pub struct #client_ident {
			gas: Option<u64>,
			address: Address,
			value: Option<U256>,
		}

		impl #client_ident {
			pub fn new(address: Address) -> Self {
				#client_ident {
					gas: None,
					address: address,
					value: None,
				}
			}

			pub fn gas(mut self, gas: u64) -> Self {
				self.gas = Some(gas);
				self
			}

			pub fn value(mut self, val: U256) -> Self {
				self.value = Some(val);
				self
			}
		}

		impl #name_ident for #client_ident {
			#client_ctor
			#(#calls)*
		}
	}
}

fn generate_eth_endpoint(endpoint_name: &str, intf: &items::Interface) -> proc_macro2::TokenStream {
	let check_value_code = quote! {
		if pwasm_ethereum::value() > 0.into() {
			panic!("Unable to accept value in non-payable constructor call");
		}
	};
	let ctor_branch = intf.constructor().map(
		|signature| {
			let arg_types = signature.arguments.iter().map(|&(_, ref ty)| quote! { #ty });
			let check_value_if_payable = if signature.is_payable { quote! {} } else { quote! {#check_value_code} };
			quote! {
				#check_value_if_payable
				let mut stream = pwasm_abi::eth::Stream::new(payload);
				self.inner.constructor(
					#(stream.pop::<#arg_types>().expect("argument decoding failed")),*
				);
			}
		}
	);

	let branches: Vec<proc_macro2::TokenStream> = intf.items().iter().filter_map(|item| {
		match *item {
			Item::Signature(ref signature)  => {
				let hash_literal = syn::Lit::Int(
					syn::LitInt::new(signature.hash as u64, syn::IntSuffix::U32, Span::call_site()));
				let ident = &signature.name;
				let arg_types = signature.arguments.iter().map(|&(_, ref ty)| quote! { #ty });
				let check_value_if_payable = if signature.is_payable { quote! {} } else { quote! {#check_value_code} };
				if !signature.return_types.is_empty() {
					let return_count_literal = syn::Lit::Int(
						syn::LitInt::new(signature.return_types.len() as u64, syn::IntSuffix::Usize, Span::call_site()));
					Some(quote! {
						#hash_literal => {
							#check_value_if_payable
							let mut stream = pwasm_abi::eth::Stream::new(method_payload);
							let result = inner.#ident(
								#(stream.pop::<#arg_types>().expect("argument decoding failed")),*
							);
							let mut sink = pwasm_abi::eth::Sink::new(#return_count_literal);
							sink.push(result);
							sink.finalize_panicking()
						}
					})
				} else {
					Some(quote! {
						#hash_literal => {
							#check_value_if_payable
							let mut stream = pwasm_abi::eth::Stream::new(method_payload);
							inner.#ident(
								#(stream.pop::<#arg_types>().expect("argument decoding failed")),*
							);
							Vec::new()
						}
					})
				}
			},
			_ => None,
		}
	}).collect();

	let endpoint_ident = syn::Ident::new(endpoint_name, Span::call_site());
	let name_ident = syn::Ident::new(&intf.name(), Span::call_site());

	quote! {
		pub struct #endpoint_ident<T: #name_ident> {
			pub inner: T,
		}

		impl<T: #name_ident> From<T> for #endpoint_ident<T> {
			fn from(inner: T) -> #endpoint_ident<T> {
				#endpoint_ident {
					inner: inner,
				}
			}
		}

		impl<T: #name_ident> #endpoint_ident<T> {
			pub fn new(inner: T) -> Self {
				#endpoint_ident {
					inner: inner,
				}
			}

			pub fn instance(&self) -> &T {
				&self.inner
			}
		}

		impl<T: #name_ident> pwasm_abi::eth::EndpointInterface for #endpoint_ident<T> {
			#[allow(unused_mut)]
			#[allow(unused_variables)]
			fn dispatch(&mut self, payload: &[u8]) -> Vec<u8> {
				let inner = &mut self.inner;
				if payload.len() < 4 {
					panic!("Invalid abi invoke");
				}
				let method_id = ((payload[0] as u32) << 24)
					+ ((payload[1] as u32) << 16)
					+ ((payload[2] as u32) << 8)
					+ (payload[3] as u32);

				let method_payload = &payload[4..];

				match method_id {
					#(#branches,)*
					_ => panic!("Invalid method signature"),
				}
			}

			#[allow(unused_variables)]
			#[allow(unused_mut)]
			fn dispatch_ctor(&mut self, payload: &[u8]) {
				#ctor_branch
			}
		}
	}
}
