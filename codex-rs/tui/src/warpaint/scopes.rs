//! The scope theme: `TextMate` scopes mapped to warpaint tokens, never to colors, so highlighted
//! text binds at flush like everything else. Tokens travel in syntect's bat-compatible ANSI
//! encoding (alpha 0: `r` is a palette index), which upstream's converter already turns into
//! `Color::Indexed`. One theme serves every pack; only its name, the pack's, differs.

use std::sync::OnceLock;

use syntect::highlighting::Color;
use syntect::highlighting::ScopeSelectors;
use syntect::highlighting::StyleModifier;
use syntect::highlighting::Theme;
use syntect::highlighting::ThemeItem;
use syntect::highlighting::ThemeSettings;

use super::paint::Figure;
use super::paint::Syntax;

/// Alpha 1: the terminal's default color, which unscoped text inherits from its surroundings.
const INHERITED: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 1,
};

const fn token(token: u8) -> Color {
    Color {
        r: token,
        g: 0,
        b: 0,
        a: 0,
    }
}

/// Specific `TextMate` scopes and overrides, not just ten broad classes. Generic rules precede
/// punctuation inside strings and comments and language-specific exceptions.
pub(super) const RULES: &[(u8, &str)] = &[
    (
        Syntax::Variable.token(),
        "variable, support.variable, meta.definition.variable, meta.object-literal.key, meta.mapping.key",
    ),
    (
        Syntax::Constant.token(),
        "constant, constant.language, constant.other, support.constant, variable.other.constant, entity.name.constant",
    ),
    (
        Syntax::Type.token(),
        "entity.other.inherited-class, entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.interface, entity.name.namespace, entity.name.module, support.type, support.class, storage.type, storage.modifier.lifetime",
    ),
    (
        Syntax::Function.token(),
        "entity.name.function, support.function, variable.function, meta.function-call, meta.method-call, entity.name.function.macro, support.macro",
    ),
    (
        Syntax::Keyword.token(),
        "keyword, storage, keyword.control, keyword.other, storage.modifier, storage.type.function, storage.type.class, storage.type.struct, storage.type.enum, storage.type.trait, storage.type.impl",
    ),
    (
        Syntax::Punctuation.token(),
        "punctuation, keyword.operator, meta.brace, punctuation.separator, punctuation.terminator, punctuation.accessor, punctuation.section, punctuation.definition.tag",
    ),
    (
        Syntax::String.token(),
        "string, string.quoted, string.unquoted, string.regexp, punctuation.definition.string, punctuation.definition.string.begin, punctuation.definition.string.end, punctuation.definition.string.template",
    ),
    (
        Figure::Code.token(),
        "markup.inline.raw.string.markdown, markup.raw.inline.markdown, markup.raw",
    ),
    (
        Syntax::Number.token(),
        "constant.numeric, constant.numeric.integer, constant.numeric.float, constant.numeric.hex, constant.numeric.binary",
    ),
    (
        Syntax::Attribute.token(),
        "entity.other.attribute-name, entity.name.tag, meta.annotation, punctuation.definition.annotation, variable.annotation, support.type.annotation, entity.name.function.decorator, meta.decorator, constant.other.placeholder, constant.character.escape, punctuation.definition.interpolation, punctuation.section.interpolation",
    ),
    (
        Syntax::Comment.token(),
        "comment, punctuation.definition.comment, comment.line, comment.block, comment.block.documentation, comment.line.documentation",
    ),
    (
        Figure::Ink.token(),
        "markup.heading, entity.name.section, markup.bold, markup.italic",
    ),
    (
        Figure::Link.token(),
        "markup.underline.link, string.other.link, meta.link",
    ),
    (Figure::Added.token(), "markup.inserted, diff.inserted"),
    (Figure::Removed.token(), "markup.deleted, diff.deleted"),
    (Figure::Caution.token(), "markup.changed, diff.changed"),
    (
        Figure::Failure.token(),
        "invalid, invalid.illegal, invalid.deprecated",
    ),
];

#[allow(
    clippy::expect_used,
    reason = "the selectors are literals in this file; one that fails to parse is a defect here"
)]
fn scopes() -> &'static [ThemeItem] {
    static SCOPES: OnceLock<Vec<ThemeItem>> = OnceLock::new();
    SCOPES.get_or_init(|| {
        RULES
            .iter()
            .map(|&(paint, selectors)| ThemeItem {
                scope: selectors
                    .parse::<ScopeSelectors>()
                    .expect("warpaint scope selectors parse"),
                style: StyleModifier {
                    foreground: Some(token(paint)),
                    ..StyleModifier::default()
                },
            })
            .collect()
    })
}

/// The scope theme, named for the pack it is resolved from.
pub(super) fn theme(pack: &str) -> Theme {
    Theme {
        name: Some(pack.to_owned()),
        settings: ThemeSettings {
            foreground: Some(INHERITED),
            ..ThemeSettings::default()
        },
        scopes: scopes().to_vec(),
        ..Theme::default()
    }
}
