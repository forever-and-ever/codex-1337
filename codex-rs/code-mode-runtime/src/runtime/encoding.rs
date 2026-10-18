use std::cell::Cell;
use std::collections::HashMap;

use base64::Engine;
use base64::alphabet::STANDARD;
use base64::engine::DecodePaddingMode;
use base64::engine::GeneralPurpose;
use base64::engine::GeneralPurposeConfig;
use encoding_rs::CoderResult;
use encoding_rs::Decoder;
use encoding_rs::DecoderResult;
use encoding_rs::Encoding;

#[derive(Default)]
struct Decoders {
    next_id: u64,
    // No strong JS root: GC releases dead decoders; isolate teardown owns the rest.
    values: HashMap<u64, (DecoderState, v8::Weak<v8::Function>)>,
}

struct DecoderState {
    encoding: &'static Encoding,
    fatal: bool,
    ignore_bom: bool,
    active: Option<Decoder>,
}

enum Fault {
    Type(&'static str),
    Range(&'static str),
    InvalidCharacter,
}

type NativeResult<'s> = Result<v8::Local<'s, v8::Value>, Fault>;

const ALLOCATION: Fault = Fault::Range("Encoding allocation exceeds the runtime limit");
const STATE: Fault = Fault::Type("Encoding runtime state is unavailable");

macro_rules! callbacks {
    ($($callback:ident => $implementation:ident),* $(,)?) => {$(
        #[allow(clippy::needless_pass_by_value, reason = "V8 callback ABI owns its arguments")]
        fn $callback(
            scope: &mut v8::PinScope<'_, '_>,
            args: v8::FunctionCallbackArguments<'_>,
            mut rv: v8::ReturnValue<'_, v8::Value>,
        ) {
            match $implementation(scope, &args) {
                Ok(value) => rv.set(value),
                Err(error) => error.throw(scope),
            }
        }
    )*};
}

callbacks! {
    encode_callback => encode,
    decoder_callback => decoder,
    decode_callback => decode,
    atob_callback => atob,
    btoa_callback => btoa,
}

impl Fault {
    fn throw(self, scope: &mut v8::PinScope<'_, '_>) {
        if scope.is_execution_terminating() {
            return;
        }
        let message = match self {
            Self::Type(message) | Self::Range(message) => message,
            Self::InvalidCharacter => "Invalid character",
        };
        let Some(message) = v8::String::new(scope, message) else {
            return;
        };
        let error = match self {
            Self::Type(_) => v8::Exception::type_error(scope, message),
            Self::Range(_) => v8::Exception::range_error(scope, message),
            Self::InvalidCharacter => {
                let error = v8::Exception::error(scope, message);
                let Some(name) = v8::String::new(scope, "name") else {
                    return;
                };
                let Some(value) = v8::String::new(scope, "InvalidCharacterError") else {
                    return;
                };
                if let Some(object) = error.to_object(scope) {
                    object.set(scope, name.into(), value.into());
                }
                error
            }
        };
        scope.throw_exception(error);
    }
}

pub(super) fn install_globals(scope: &mut v8::PinScope<'_, '_>) -> Result<(), String> {
    scope.set_slot(Decoders::default());
    let source = v8::String::new(scope, include_str!("encoding.js"))
        .ok_or_else(|| "failed to allocate encoding globals".to_owned())?;
    let factory = v8::Script::compile(scope, source, None)
        .and_then(|script| script.run(scope))
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
        .ok_or_else(|| "failed to compile encoding globals".to_owned())?;
    let encode = v8::Function::new(scope, encode_callback)
        .ok_or_else(|| "failed to allocate UTF-8 encoder".to_owned())?;
    let decoder = v8::Function::new(scope, decoder_callback)
        .ok_or_else(|| "failed to allocate text decoder".to_owned())?;
    let atob = v8::Function::new(scope, atob_callback)
        .ok_or_else(|| "failed to allocate atob".to_owned())?;
    let btoa = v8::Function::new(scope, btoa_callback)
        .ok_or_else(|| "failed to allocate btoa".to_owned())?;
    let receiver = v8::undefined(scope);
    factory
        .call(
            scope,
            receiver.into(),
            &[encode.into(), decoder.into(), atob.into(), btoa.into()],
        )
        .ok_or_else(|| "failed to initialize encoding globals".to_owned())?;
    Ok(())
}

fn encode<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'_>,
) -> NativeResult<'s> {
    let source = v8::Local::<v8::String>::try_from(args.get(0))
        .map_err(|_| Fault::Type("Encoder input must be a string"))?;
    let capacity = if args.length() == 1 {
        source.utf8_length(scope)
    } else {
        let destination = v8::Local::<v8::Uint8Array>::try_from(args.get(1))
            .map_err(|_| Fault::Type("Destination must be a Uint8Array"))?;
        if destination
            .buffer(scope)
            .is_none_or(|buffer| buffer.was_detached())
        {
            return Err(Fault::Type("Destination buffer is detached"));
        }
        source.utf8_length(scope).min(destination.byte_length())
    };
    let mut bytes = vec![0; capacity];
    let mut read = 0;
    let written = source.write_utf8_v2(
        scope,
        &mut bytes,
        v8::WriteFlags::kReplaceInvalidUtf8,
        Some(&mut read),
    );
    bytes.truncate(written);
    let buffer = v8::ArrayBuffer::new(scope, written);
    for (cell, byte) in buffer.get_backing_store().iter().zip(bytes) {
        cell.set(byte);
    }
    let bytes = v8::Uint8Array::new(scope, buffer, 0, written).ok_or(ALLOCATION)?;
    let read = v8::Integer::new_from_unsigned(scope, u32::try_from(read).map_err(|_| ALLOCATION)?);
    Ok(v8::Array::new_with_elements(scope, &[bytes.into(), read.into()]).into())
}

fn decoder<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'_>,
) -> NativeResult<'s> {
    let label = args.get(0).to_rust_string_lossy(scope);
    let encoding = Encoding::for_label_no_replacement(label.as_bytes())
        .ok_or(Fault::Range("Unsupported encoding label"))?;
    let state = DecoderState {
        encoding,
        fatal: args.get(1).boolean_value(scope),
        ignore_bom: args.get(2).boolean_value(scope),
        active: None,
    };
    let decoders = scope.get_slot_mut::<Decoders>().ok_or(STATE)?;
    let id = decoders.next_id;
    decoders.next_id = id.checked_add(1).ok_or(ALLOCATION)?;
    let data = v8::BigInt::new_from_u64(scope, id);
    let function = v8::Function::builder(decode_callback)
        .data(data.into())
        .build(scope)
        .ok_or(ALLOCATION)?;
    let weak = v8::Weak::with_finalizer(
        scope,
        function,
        Box::new(move |isolate| {
            if let Some(decoders) = isolate.get_slot_mut::<Decoders>() {
                decoders.values.remove(&id);
            }
        }),
    );
    scope
        .get_slot_mut::<Decoders>()
        .ok_or(STATE)?
        .values
        .insert(id, (state, weak));
    let name = v8::String::new(scope, &encoding.name().to_ascii_lowercase()).ok_or(ALLOCATION)?;
    Ok(v8::Array::new_with_elements(scope, &[name.into(), function.into()]).into())
}

fn input_bytes(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
) -> Result<Vec<u8>, Fault> {
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(value) {
        let buffer = view
            .buffer(scope)
            .ok_or(Fault::Type("Input buffer is detached"))?;
        if buffer.was_detached() {
            return Err(Fault::Type("Input buffer is detached"));
        }
        let mut bytes = vec![0; view.byte_length()];
        view.copy_contents(&mut bytes);
        return Ok(bytes);
    }
    if let Ok(buffer) = v8::Local::<v8::ArrayBuffer>::try_from(value) {
        if buffer.was_detached() {
            return Err(Fault::Type("Input buffer is detached"));
        }
        return Ok(buffer
            .get_backing_store()
            .iter()
            .take(buffer.byte_length())
            .map(Cell::get)
            .collect());
    }
    Err(Fault::Type(
        "Input must be an ArrayBuffer or ArrayBufferView",
    ))
}

fn decode<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'_>,
) -> NativeResult<'s> {
    let bytes = input_bytes(scope, args.get(0))?;
    let stream = args.get(1).boolean_value(scope);
    let id = v8::Local::<v8::BigInt>::try_from(args.data())
        .map_err(|_| STATE)?
        .u64_value()
        .0;
    let decoders = scope.get_slot_mut::<Decoders>().ok_or(STATE)?;
    let (state, _) = decoders.values.get_mut(&id).ok_or(STATE)?;
    let decoder = state.active.get_or_insert_with(|| {
        if state.ignore_bom {
            state.encoding.new_decoder_without_bom_handling()
        } else {
            state.encoding.new_decoder_with_bom_removal()
        }
    });
    let capacity = decoder
        .max_utf8_buffer_length(bytes.len())
        .ok_or(ALLOCATION)?;
    let mut text = String::with_capacity(capacity);
    let malformed = if state.fatal {
        let (result, _) = decoder.decode_to_string_without_replacement(&bytes, &mut text, !stream);
        assert!(
            !matches!(result, DecoderResult::OutputFull),
            "sized decoder output"
        );
        matches!(result, DecoderResult::Malformed(..))
    } else {
        let (result, read, _) = decoder.decode_to_string(&bytes, &mut text, !stream);
        assert_eq!(result, CoderResult::InputEmpty, "sized decoder output");
        assert_eq!(read, bytes.len(), "complete decoder input");
        false
    };
    // A failed stream restarts on the next call, including BOM and shift state.
    if !stream || malformed {
        state.active = None;
    }
    if malformed {
        return Err(Fault::Type(
            "The encoded data is not valid for this encoding",
        ));
    }
    v8::String::new(scope, &text)
        .map(Into::into)
        .ok_or(ALLOCATION)
}

fn atob<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'_>,
) -> NativeResult<'s> {
    let mut input = args.get(0).to_rust_string_lossy(scope).into_bytes();
    input.retain(|byte| !matches!(byte, b'\t' | b'\n' | b'\x0c' | b'\r' | b' '));
    if input.len().is_multiple_of(4) {
        for _ in 0..2 {
            if input.last() == Some(&b'=') {
                input.pop();
            }
        }
    }
    let engine = GeneralPurpose::new(
        &STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    let bytes = engine.decode(input).map_err(|_| Fault::InvalidCharacter)?;
    v8::String::new_from_one_byte(scope, &bytes, v8::NewStringType::Normal)
        .map(Into::into)
        .ok_or(ALLOCATION)
}

fn btoa<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'_>,
) -> NativeResult<'s> {
    let input = v8::Local::<v8::String>::try_from(args.get(0))
        .map_err(|_| Fault::Type("Base64 input must be a string"))?;
    if !input.contains_only_onebyte() {
        return Err(Fault::InvalidCharacter);
    }
    let mut bytes = vec![0; input.length()];
    input.write_one_byte_v2(scope, 0, &mut bytes, v8::WriteFlags::empty());
    let encoded = GeneralPurpose::new(&STANDARD, GeneralPurposeConfig::new()).encode(bytes);
    v8::String::new(scope, &encoded)
        .map(Into::into)
        .ok_or(ALLOCATION)
}

#[cfg(test)]
mod tests {
    use v8::ContextOptions;

    use super::Decoders;
    use super::install_globals;
    use crate::v8_init::ensure_v8_initialized;

    #[test]
    fn decoder_state_follows_its_javascript_owner() -> Result<(), String> {
        ensure_v8_initialized()?;
        let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, ContextOptions::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install_globals(scope)?;
        // Native state must survive GC while reachable without becoming a JS root itself.
        for (source, remaining) in [
            (
                r"globalThis.decoder = new TextDecoder();
                decoder.decode(new Uint8Array([195]), {stream:true});",
                1,
            ),
            (
                r#"if (decoder.decode(new Uint8Array([169])) !== "é") throw new Error("Lost stream");
                delete globalThis.decoder;"#,
                0,
            ),
        ] {
            {
                v8::scope!(let scope, scope);
                let source = v8::String::new(scope, source).ok_or("allocate GC test source")?;
                v8::Script::compile(scope, source, None)
                    .and_then(|script| script.run(scope))
                    .ok_or("evaluate GC test source")?;
            }
            scope.low_memory_notification();
            assert_eq!(
                scope
                    .get_slot::<Decoders>()
                    .map(|decoders| decoders.values.len()),
                Some(remaining)
            );
        }
        Ok(())
    }
}
