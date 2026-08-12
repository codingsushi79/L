//! The L parser.
//!
//! A hand-written recursive-descent parser with precedence climbing for binary
//! operators. It is written to keep going after an error: on a malformed
//! construct it records a diagnostic, synchronises to the next statement or
//! item boundary, and carries on, so one mistake does not mask the rest of a
//! file.
//!
//! # Statement termination
//!
//! The specification writes no semicolons, so L is newline-terminated: a
//! statement ends at a line break, a `}`, or end of file. A semicolon is
//! accepted as an explicit terminator for statements written on one line.
//! Because the lexer discards whitespace, "is there a line break here" is
//! answered by looking at the source text between two token spans.
//!
//! # Ambiguities and how they are resolved
//!
//! * `if x { ... }` versus a struct literal `X { ... }` — struct literals are
//!   disallowed in the condition of `if`/`while` and the iterable of `for`,
//!   the same restriction Rust and Swift use. Parenthesise to force one.
//! * `Box<int> { ... }` versus `a < b` — on seeing `<` after a path the parser
//!   speculatively parses a generic argument list and keeps it only if what
//!   follows is `{` or `(`.
//! * `let int age := 20` versus `let age := 20` — the parser speculatively
//!   parses a type; if a name does not follow, it rewinds and reads the first
//!   token as the variable name instead.
//! * `{ ... }` in value position is a map or set literal when its first
//!   element is an expression followed by `:`, `,` or `}`; otherwise a block.

use l_ast::*;
use l_lexer::{Keyword, StrPart, Token, TokenKind};
use l_span::{BytePos, DiagCode, Diagnostic, Diagnostics, FileId, Span};

// Parser diagnostic codes. E1xxx is reserved for syntactic errors.
const E_EXPECTED: DiagCode = DiagCode("E1001");
const E_EXPECTED_ITEM: DiagCode = DiagCode("E1002");
const E_EXPECTED_EXPR: DiagCode = DiagCode("E1003");
const E_EXPECTED_STMT_END: DiagCode = DiagCode("E1004");
const E_BAD_ASSIGN_TARGET: DiagCode = DiagCode("E1005");
const E_MODULE_POSITION: DiagCode = DiagCode("E1006");
const E_MISSING_CALL: DiagCode = DiagCode("E1007");
const E_FROM_IMPORT: DiagCode = DiagCode("E1008");
const E_EXPECTED_PATTERN: DiagCode = DiagCode("E1009");
const E_EMPTY_MATCH: DiagCode = DiagCode("E1010");
const E_TRY_NO_CATCH: DiagCode = DiagCode("E1011");
const E_BAD_TUPLE_INDEX: DiagCode = DiagCode("E1012");
const E_C_STYLE_FOR: DiagCode = DiagCode("E1013");

/// The result of parsing one file.
pub struct Parsed {
    pub unit: SourceUnit,
    pub diagnostics: Diagnostics,
}

/// Parse a source file that has already been lexed.
pub fn parse(file: FileId, src: &str, tokens: Vec<Token>) -> Parsed {
    let mut parser = Parser::new(file, src, tokens);
    let unit = parser.parse_source_unit();
    Parsed { unit, diagnostics: parser.diagnostics }
}

/// Lex and parse a source file in one step.
pub fn parse_source(file: FileId, src: &str) -> Parsed {
    let lexed = l_lexer::lex(file, src);
    let mut parsed = parse(file, src, lexed.tokens);
    let mut diags = lexed.diagnostics;
    diags.extend(parsed.diagnostics);
    parsed.diagnostics = diags;
    parsed
}

struct Parser<'a> {
    file: FileId,
    src: &'a str,
    tokens: Vec<Token>,
    /// Index of the next token to consume.
    pos: usize,
    next_id: u32,
    diagnostics: Diagnostics,
    /// Set while parsing the head of `if`/`while`/`for`, where a `{` begins the
    /// body rather than a struct literal.
    no_struct_lit: bool,
    /// Guards against emitting a cascade of errors at the same token.
    last_error_pos: Option<usize>,
}

impl<'a> Parser<'a> {
    fn new(file: FileId, src: &'a str, tokens: Vec<Token>) -> Self {
        Parser {
            file,
            src,
            tokens,
            pos: 0,
            next_id: 0,
            diagnostics: Diagnostics::new(),
            no_struct_lit: false,
            last_error_pos: None,
        }
    }

    // ---- ids and spans ----

    fn next_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    fn span_from(&self, lo: Span) -> Span {
        lo.to(self.prev().span)
    }

    // ---- token access ----

    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn peek_at(&self, n: usize) -> &Token {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)]
    }

    fn prev(&self) -> &Token {
        &self.tokens[self.pos.saturating_sub(1).min(self.tokens.len() - 1)]
    }

    fn at_end(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Eof)
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if !self.at_end() {
            self.pos += 1;
        }
        token
    }

    fn check(&self, kind: &TokenKind) -> bool {
        self.peek_kind() == kind
    }

    fn check_kw(&self, kw: Keyword) -> bool {
        self.peek_kind().is_keyword(kw)
    }

    /// Consume the token if it matches.
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: Keyword) -> bool {
        if self.check_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume `kind`, or report that it was expected.
    fn expect(&mut self, kind: &TokenKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let found = self.peek_kind().describe();
        let span = self.peek().span;
        self.error_at(
            span,
            Diagnostic::error(
                E_EXPECTED,
                format!("expected `{}`, found {found}", kind.punct_str()),
            )
            .with_primary(span, format!("expected `{}`", kind.punct_str())),
        );
        false
    }

    /// Whether a line break separates the previous token from the next one.
    fn newline_before(&self) -> bool {
        let end = self.prev().span.hi as usize;
        let start = self.peek().span.lo as usize;
        if start <= end || end > self.src.len() || start > self.src.len() {
            return false;
        }
        self.src[end..start].contains('\n')
    }

    // ---- diagnostics ----

    fn error_at(&mut self, _span: Span, diag: Diagnostic) {
        let pos = self.pos;
        self.error_at_pos(pos, diag);
    }

    /// Record a diagnostic, unless one was already reported at this token.
    fn error_at_pos(&mut self, pos: usize, diag: Diagnostic) {
        if self.last_error_pos == Some(pos) {
            return;
        }
        self.last_error_pos = Some(pos);
        self.diagnostics.push(diag);
    }

    /// Skip tokens until the start of something that plausibly begins a new
    /// item, so parsing can resume.
    fn sync_to_item(&mut self) {
        let mut depth = 0i32;
        while !self.at_end() {
            match self.peek_kind() {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth < 0 {
                        return;
                    }
                }
                TokenKind::At if depth == 0 => return,
                TokenKind::Keyword(kw) if depth == 0 => {
                    if matches!(
                        kw,
                        Keyword::Fn
                            | Keyword::Struct
                            | Keyword::Enum
                            | Keyword::Interface
                            | Keyword::Impl
                            | Keyword::Const
                            | Keyword::Use
                            | Keyword::Pub
                            | Keyword::Module
                            | Keyword::Extern
                            | Keyword::Async
                    ) {
                        return;
                    }
                }
                _ => {}
            }
            self.advance();
        }
    }

    /// Skip to the end of the current statement.
    fn sync_to_stmt(&mut self) {
        let mut depth = 0i32;
        while !self.at_end() {
            match self.peek_kind() {
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RBrace if depth == 0 => return,
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => depth -= 1,
                TokenKind::Semi if depth == 0 => {
                    self.advance();
                    return;
                }
                _ => {}
            }
            self.advance();
            if depth <= 0 && self.newline_before() {
                return;
            }
        }
    }

    // =======================================================================
    // Source unit (SPEC §33, §34, §35)
    // =======================================================================

    fn parse_source_unit(&mut self) -> SourceUnit {
        let id = self.next_id();
        let lo = self.peek().span;

        let mut docs = self.parse_docs();

        // `module users` must come first if present (SPEC §33).
        let module = if self.check_kw(Keyword::Module) {
            let mod_lo = self.peek().span;
            self.advance();
            let name = self.parse_ident("module name");
            let mod_id = self.next_id();
            Some(ModuleDecl {
                id: mod_id,
                name,
                docs: std::mem::take(&mut docs),
                span: self.span_from(mod_lo),
            })
        } else {
            None
        };

        let mut uses = Vec::new();
        let mut items = Vec::new();

        loop {
            if !docs.is_empty() {
                // Docs were read ahead of the loop; keep them for the next item.
            } else {
                docs = self.parse_docs();
            }

            if self.at_end() {
                break;
            }

            if self.check_kw(Keyword::Module) {
                let span = self.peek().span;
                self.error_at(
                    span,
                    Diagnostic::error(E_MODULE_POSITION, "`module` must be the first declaration")
                        .with_primary(span, "unexpected module declaration")
                        .with_note("a file declares at most one module, before any other item"),
                );
                self.advance();
                let _ = self.parse_ident("module name");
                docs = Docs::default();
                continue;
            }

            if self.check_kw(Keyword::Use) {
                uses.push(self.parse_use());
                docs = Docs::default();
                continue;
            }

            match self.parse_item(std::mem::take(&mut docs)) {
                Some(item) => items.push(item),
                None => {
                    let span = self.peek().span;
                    let found = self.peek_kind().describe();
                    self.error_at(
                        span,
                        Diagnostic::error(
                            E_EXPECTED_ITEM,
                            format!("expected a declaration, found {found}"),
                        )
                        .with_primary(span, "not a declaration")
                        .with_note(
                            "the top level of a file may contain `use`, `fn`, `struct`, `enum`, \
                             `interface`, `impl` and `const` declarations",
                        ),
                    );
                    self.advance();
                    self.sync_to_item();
                }
            }
        }

        SourceUnit { id, module, uses, items, span: self.span_from(lo) }
    }

    /// Collect any run of `///` comments.
    fn parse_docs(&mut self) -> Docs {
        let mut lines = Vec::new();
        let mut span: Option<Span> = None;
        while let TokenKind::DocComment(text) = self.peek_kind() {
            let text = text.clone();
            let tok_span = self.peek().span;
            span = Some(match span {
                Some(s) => s.to(tok_span),
                None => tok_span,
            });
            lines.push(text);
            self.advance();
        }
        Docs { lines, span }
    }

    /// `@test`, `@inline`, `@deprecated("...")` (SPEC §73).
    fn parse_attributes(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        while self.check(&TokenKind::At) {
            let lo = self.peek().span;
            self.advance();
            let name = self.parse_ident("attribute name");
            let mut args = Vec::new();
            if self.eat(&TokenKind::LParen) {
                while !self.check(&TokenKind::RParen) && !self.at_end() {
                    args.push(self.parse_expr());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen);
            }
            attrs.push(Attribute { name, args, span: self.span_from(lo) });
        }
        attrs
    }

    /// `use math.sqrt as root, math.sin` (SPEC §34).
    fn parse_use(&mut self) -> Use {
        let id = self.next_id();
        let lo = self.peek().span;
        self.advance(); // `use`

        let mut trees = Vec::new();
        loop {
            let tree_lo = self.peek().span;
            let path = self.parse_path();

            // Catch the Python form the spec explicitly rejects (§2.2, §34).
            if self.check_kw(Keyword::In) || matches!(self.peek_kind(), TokenKind::Ident(n) if n == "import")
            {
                let span = self.peek().span;
                self.error_at(
                    span,
                    Diagnostic::error(E_FROM_IMPORT, "L does not have `from ... import ...`")
                        .with_primary(span, "unexpected here")
                        .with_note("write `use module.symbol` instead (SPEC §2.2)")
                        .with_suggestion(
                            "for example",
                            span,
                            format!("use {}.Symbol", path.to_string_dotted()),
                        ),
                );
                self.sync_to_stmt();
            }

            let alias = if self.eat_kw(Keyword::As) {
                Some(self.parse_ident("alias"))
            } else {
                None
            };

            let tree_id = self.next_id();
            trees.push(UseTree { id: tree_id, path, alias, span: self.span_from(tree_lo) });

            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }

        self.expect_stmt_end("use declaration");
        Use { id, trees, span: self.span_from(lo) }
    }

    fn parse_path(&mut self) -> Path {
        let lo = self.peek().span;
        let mut segments = vec![self.parse_ident("name")];
        while self.check(&TokenKind::Dot) {
            // Only continue the path while the next token is a plain name.
            if !matches!(self.peek_at(1).kind, TokenKind::Ident(_)) {
                break;
            }
            self.advance();
            segments.push(self.parse_ident("name"));
        }
        Path { segments, span: self.span_from(lo) }
    }

    fn parse_ident(&mut self, what: &str) -> Ident {
        match self.peek_kind().clone() {
            TokenKind::Ident(name) => {
                let span = self.peek().span;
                self.advance();
                Ident::new(name, span)
            }
            // `self` is a name in receiver position (SPEC §27).
            TokenKind::Keyword(Keyword::Self_) => {
                let span = self.peek().span;
                self.advance();
                Ident::new("self", span)
            }
            other => {
                let span = self.peek().span;
                self.error_at(
                    span,
                    Diagnostic::error(
                        E_EXPECTED,
                        format!("expected {what}, found {}", other.describe()),
                    )
                    .with_primary(span, format!("expected {what}")),
                );
                Ident::new("<error>", span)
            }
        }
    }

    /// Require a statement terminator: newline, `}`, `;` or end of file.
    fn expect_stmt_end(&mut self, what: &str) {
        if self.eat(&TokenKind::Semi) {
            return;
        }
        if self.at_end() || self.check(&TokenKind::RBrace) || self.newline_before() {
            return;
        }
        let span = self.peek().span;
        let found = self.peek_kind().describe();
        self.error_at(
            span,
            Diagnostic::error(
                E_EXPECTED_STMT_END,
                format!("expected end of {what}, found {found}"),
            )
            .with_primary(span, "unexpected here")
            .with_note("statements in L end at a line break; use `;` to write two on one line"),
        );
        self.sync_to_stmt();
    }

    // =======================================================================
    // Items
    // =======================================================================

    fn parse_item(&mut self, docs: Docs) -> Option<Item> {
        let attrs = self.parse_attributes();
        // Attributes may be followed by their own doc comments.
        let docs = if docs.is_empty() { self.parse_docs() } else { docs };

        let lo = self.peek().span;
        let vis = if self.check_kw(Keyword::Pub) {
            let span = self.peek().span;
            self.advance();
            Visibility::Public(span)
        } else {
            Visibility::Private
        };

        let id = self.next_id();

        let kind = if self.check_kw(Keyword::Fn)
            || self.check_kw(Keyword::Async)
            || self.check_kw(Keyword::Extern)
        {
            ItemKind::Fn(Box::new(self.parse_fn_decl()?))
        } else if self.check_kw(Keyword::Struct) {
            ItemKind::Struct(Box::new(self.parse_struct_decl()))
        } else if self.check_kw(Keyword::Enum) {
            ItemKind::Enum(Box::new(self.parse_enum_decl()))
        } else if self.check_kw(Keyword::Interface) {
            ItemKind::Interface(Box::new(self.parse_interface_decl()))
        } else if self.check_kw(Keyword::Impl) {
            ItemKind::Impl(Box::new(self.parse_impl_block()))
        } else if self.check_kw(Keyword::Const) {
            let decl = self.parse_const_decl();
            self.expect_stmt_end("constant declaration");
            ItemKind::Const(Box::new(decl))
        } else {
            // Not an item. Rewind the visibility token so the caller sees it.
            if vis.is_public() {
                self.pos -= 1;
            }
            return None;
        };

        Some(Item { id, kind, vis, attrs, docs, span: self.span_from(lo) })
    }

    /// `async fn f(...) -> T { }`, `fn User.greet()`, `extern fn printf(...)`.
    fn parse_fn_decl(&mut self) -> Option<FnDecl> {
        let is_async = self.eat_kw(Keyword::Async);
        let is_extern = self.eat_kw(Keyword::Extern);

        if !self.expect(&TokenKind::Keyword(Keyword::Fn)) {
            self.sync_to_item();
            return None;
        }

        let first = self.parse_ident("function name");

        // `fn User.greet()` — a method (SPEC §27).
        let (receiver, name) = if self.check(&TokenKind::Dot) {
            self.advance();
            (Some(first), self.parse_ident("method name"))
        } else {
            (None, first)
        };

        let generics = self.parse_generic_params();

        let mut params = Vec::new();
        let mut is_variadic = false;
        self.expect(&TokenKind::LParen);
        while !self.check(&TokenKind::RParen) && !self.at_end() {
            // Trailing `...` in an extern declaration (SPEC §72).
            if self.check(&TokenKind::Ellipsis) {
                self.advance();
                is_variadic = true;
                break;
            }
            let param_lo = self.peek().span;
            let ty = self.parse_type();
            let pname = self.parse_ident("parameter name");
            let param_id = self.next_id();
            params.push(Param {
                id: param_id,
                ty,
                name: pname,
                span: self.span_from(param_lo),
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen);

        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };

        // A body, unless this is an extern or interface signature.
        let body = if self.check(&TokenKind::LBrace) {
            Some(self.parse_block())
        } else {
            self.expect_stmt_end("function declaration");
            None
        };

        Some(FnDecl { receiver, name, generics, params, ret, body, is_async, is_extern, is_variadic })
    }

    /// `<T>` or `<T: Printable, U>` (SPEC §29).
    fn parse_generic_params(&mut self) -> Vec<GenericParam> {
        let mut params = Vec::new();
        if !self.check(&TokenKind::Lt) {
            return params;
        }
        self.advance();
        while !self.check(&TokenKind::Gt) && !self.at_end() {
            let lo = self.peek().span;
            let name = self.parse_ident("type parameter");
            let mut bounds = Vec::new();
            if self.eat(&TokenKind::Colon) {
                loop {
                    bounds.push(self.parse_path());
                    if !self.eat(&TokenKind::Plus) {
                        break;
                    }
                }
            }
            let id = self.next_id();
            params.push(GenericParam { id, name, bounds, span: self.span_from(lo) });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::Gt);
        params
    }

    /// `struct User { str name  int age := 0 }` (SPEC §23, §24).
    fn parse_struct_decl(&mut self) -> StructDecl {
        self.advance(); // `struct`
        let name = self.parse_ident("struct name");
        let generics = self.parse_generic_params();
        let mut fields = Vec::new();

        self.expect(&TokenKind::LBrace);
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            let docs = self.parse_docs();
            if self.check(&TokenKind::RBrace) {
                break;
            }
            let lo = self.peek().span;
            let vis = if self.check_kw(Keyword::Pub) {
                let span = self.peek().span;
                self.advance();
                Visibility::Public(span)
            } else {
                Visibility::Private
            };
            let ty = self.parse_type();
            let fname = self.parse_ident("field name");
            let default = if self.eat(&TokenKind::Assign) {
                Some(self.parse_expr())
            } else {
                None
            };
            let id = self.next_id();
            fields.push(Field {
                id,
                vis,
                ty,
                name: fname,
                default,
                docs,
                span: self.span_from(lo),
            });
            // Fields are separated by newlines; a comma is tolerated.
            self.eat(&TokenKind::Comma);
        }
        self.expect(&TokenKind::RBrace);

        StructDecl { name, generics, fields }
    }

    /// `enum Message { TEXT(str)  QUIT }` (SPEC §25).
    fn parse_enum_decl(&mut self) -> EnumDecl {
        self.advance(); // `enum`
        let name = self.parse_ident("enum name");
        let generics = self.parse_generic_params();
        let mut variants = Vec::new();

        self.expect(&TokenKind::LBrace);
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            let docs = self.parse_docs();
            if self.check(&TokenKind::RBrace) {
                break;
            }
            let lo = self.peek().span;
            let vname = self.parse_ident("variant name");
            let mut payload = Vec::new();
            if self.eat(&TokenKind::LParen) {
                while !self.check(&TokenKind::RParen) && !self.at_end() {
                    payload.push(self.parse_type());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen);
            }
            let id = self.next_id();
            variants.push(Variant { id, name: vname, payload, docs, span: self.span_from(lo) });
            self.eat(&TokenKind::Comma);
        }
        self.expect(&TokenKind::RBrace);

        EnumDecl { name, generics, variants }
    }

    /// `interface Printable { fn print() }` (SPEC §28).
    fn parse_interface_decl(&mut self) -> InterfaceDecl {
        self.advance(); // `interface`
        let name = self.parse_ident("interface name");
        let generics = self.parse_generic_params();
        let methods = self.parse_method_block();
        InterfaceDecl { name, generics, methods }
    }

    /// `impl Printable for User { ... }` (SPEC §28).
    fn parse_impl_block(&mut self) -> ImplBlock {
        self.advance(); // `impl`
        let generics = self.parse_generic_params();
        let interface = self.parse_path();

        // `for` is a keyword; the spec writes `impl Printable for User`.
        let target = if self.eat_kw(Keyword::For) {
            self.parse_type()
        } else {
            let span = self.peek().span;
            self.error_at(
                span,
                Diagnostic::error(E_EXPECTED, "expected `for` in impl declaration")
                    .with_primary(span, "expected `for`")
                    .with_note("implementations are written `impl Interface for Type { ... }`"),
            );
            Type { id: self.next_id(), kind: TypeKind::Err, span }
        };

        let methods = self.parse_method_block();
        ImplBlock { interface, target, generics, methods }
    }

    /// The `{ ... }` body of an interface or impl, containing only functions.
    fn parse_method_block(&mut self) -> Vec<Item> {
        let mut methods = Vec::new();
        self.expect(&TokenKind::LBrace);
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            let docs = self.parse_docs();
            if self.check(&TokenKind::RBrace) {
                break;
            }
            match self.parse_item(docs) {
                Some(item) => methods.push(item),
                None => {
                    let span = self.peek().span;
                    let found = self.peek_kind().describe();
                    self.error_at(
                        span,
                        Diagnostic::error(
                            E_EXPECTED_ITEM,
                            format!("expected a method, found {found}"),
                        )
                        .with_primary(span, "only `fn` declarations are allowed here"),
                    );
                    self.advance();
                    self.sync_to_item();
                }
            }
        }
        self.expect(&TokenKind::RBrace);
        methods
    }

    /// `const int MAX_USERS := 100` (SPEC §8).
    fn parse_const_decl(&mut self) -> ConstDecl {
        self.advance(); // `const`
        let (ty, name) = self.parse_typed_binding("constant name");
        self.expect(&TokenKind::Assign);
        let value = self.parse_expr();
        ConstDecl { ty, name, value }
    }

    /// Parse the `[type] name` prefix shared by `let` and `const`.
    ///
    /// The type is optional (SPEC §7). Since both a type and a name are bare
    /// identifiers, this speculatively parses a type and rewinds if no name
    /// follows it.
    fn parse_typed_binding(&mut self, what: &str) -> (Option<Type>, Ident) {
        let start = self.pos;
        let start_ids = self.next_id;
        let start_diags = self.diagnostics.len();

        // A single identifier followed by `:=` is a name, not a type.
        if matches!(self.peek_kind(), TokenKind::Ident(_))
            && matches!(self.peek_at(1).kind, TokenKind::Assign)
        {
            let name = self.parse_ident(what);
            return (None, name);
        }

        let ty = self.parse_type();
        if matches!(self.peek_kind(), TokenKind::Ident(_)) && !ty.is_err() {
            let name = self.parse_ident(what);
            return (Some(ty), name);
        }

        // No name followed: what we read was the name after all.
        self.pos = start;
        self.next_id = start_ids;
        self.truncate_diagnostics(start_diags);
        let name = self.parse_ident(what);
        (None, name)
    }

    /// Drop diagnostics recorded during a speculative parse.
    fn truncate_diagnostics(&mut self, len: usize) {
        let kept: Vec<_> = self.diagnostics.clone().into_vec().into_iter().take(len).collect();
        let mut diags = Diagnostics::new();
        for d in kept {
            diags.push(d);
        }
        self.diagnostics = diags;
        self.last_error_pos = None;
    }

    // =======================================================================
    // Types
    // =======================================================================

    fn parse_type(&mut self) -> Type {
        let lo = self.peek().span;
        let id = self.next_id();

        let mut kind = match self.peek_kind().clone() {
            // `(A, B)` — a tuple type (SPEC §15).
            TokenKind::LParen => {
                self.advance();
                let mut items = Vec::new();
                while !self.check(&TokenKind::RParen) && !self.at_end() {
                    items.push(self.parse_type());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen);
                if items.is_empty() {
                    TypeKind::Void
                } else if items.len() == 1 {
                    // `(T)` is just `T`.
                    items.pop().expect("checked len").kind
                } else {
                    TypeKind::Tuple(items)
                }
            }

            // `fn(A) -> B`
            TokenKind::Keyword(Keyword::Fn) => {
                self.advance();
                self.expect(&TokenKind::LParen);
                let mut params = Vec::new();
                while !self.check(&TokenKind::RParen) && !self.at_end() {
                    params.push(self.parse_type());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen);
                let ret = if self.eat(&TokenKind::Arrow) {
                    Some(Box::new(self.parse_type()))
                } else {
                    None
                };
                TypeKind::Fn { params, ret }
            }

            TokenKind::Ident(name) => {
                let path = self.parse_path();
                // `map<K, V>` and `set<T>` are spelled like generics (§13, §14).
                let generics = self.parse_generic_args();
                match (name.as_str(), generics.len()) {
                    ("map", 2) if path.is_single() => {
                        let mut it = generics.into_iter();
                        let k = it.next().expect("len checked");
                        let v = it.next().expect("len checked");
                        TypeKind::Map(Box::new(k), Box::new(v))
                    }
                    ("set", 1) if path.is_single() => {
                        let t = generics.into_iter().next().expect("len checked");
                        TypeKind::Set(Box::new(t))
                    }
                    _ => TypeKind::Named { path, generics },
                }
            }

            other => {
                let span = self.peek().span;
                self.error_at(
                    span,
                    Diagnostic::error(
                        E_EXPECTED,
                        format!("expected a type, found {}", other.describe()),
                    )
                    .with_primary(span, "expected a type"),
                );
                TypeKind::Err
            }
        };

        // Suffixes: `T[]` (§12) and `T?` (§30), in any order.
        loop {
            if self.check(&TokenKind::LBracket) && matches!(self.peek_at(1).kind, TokenKind::RBracket)
            {
                self.advance();
                self.advance();
                let inner = Type { id: self.next_id(), kind, span: self.span_from(lo) };
                kind = TypeKind::Array(Box::new(inner));
            } else if self.check(&TokenKind::Question) {
                self.advance();
                let inner = Type { id: self.next_id(), kind, span: self.span_from(lo) };
                kind = TypeKind::Optional(Box::new(inner));
            } else {
                break;
            }
        }

        Type { id, kind, span: self.span_from(lo) }
    }

    /// `<int, str>` in type position. Returns empty if there is no `<`.
    fn parse_generic_args(&mut self) -> Vec<Type> {
        let mut args = Vec::new();
        if !self.check(&TokenKind::Lt) {
            return args;
        }
        self.advance();
        while !self.check(&TokenKind::Gt) && !self.at_end() {
            args.push(self.parse_type());
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        // `Box<Box<int>>` closes with `>>`, which lexes as a shift.
        if self.check(&TokenKind::Shr) {
            self.split_shr();
        }
        self.expect(&TokenKind::Gt);
        args
    }

    /// Split a `>>` token into two `>` tokens, for nested generic arguments.
    fn split_shr(&mut self) {
        let span = self.peek().span;
        let mid = span.lo + 1;
        self.tokens[self.pos] = Token::new(TokenKind::Gt, Span::new(self.file, span.lo, mid));
        self.tokens.insert(
            self.pos + 1,
            Token::new(TokenKind::Gt, Span::new(self.file, mid, span.hi)),
        );
    }

    // =======================================================================
    // Blocks and statements
    // =======================================================================

    fn parse_block(&mut self) -> Block {
        let id = self.next_id();
        let lo = self.peek().span;
        // Struct-literal restriction applies only to the head, not the body.
        let saved = std::mem::replace(&mut self.no_struct_lit, false);

        self.expect(&TokenKind::LBrace);
        let mut stmts = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            let before = self.pos;
            match self.parse_stmt() {
                Some(stmt) => stmts.push(stmt),
                None => {}
            }
            // Guarantee progress even if a sub-parser consumed nothing.
            if self.pos == before {
                self.advance();
            }
        }
        self.expect(&TokenKind::RBrace);

        self.no_struct_lit = saved;
        Block { id, stmts, span: self.span_from(lo) }
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let docs = self.parse_docs();
        if self.check(&TokenKind::RBrace) || self.at_end() {
            return None;
        }

        let lo = self.peek().span;
        let id = self.next_id();

        // Items may be declared inside a body.
        if self.check(&TokenKind::At)
            || self.check_kw(Keyword::Fn)
            || self.check_kw(Keyword::Struct)
            || self.check_kw(Keyword::Enum)
            || self.check_kw(Keyword::Interface)
            || self.check_kw(Keyword::Impl)
        {
            if let Some(item) = self.parse_item(docs) {
                return Some(Stmt {
                    id,
                    kind: StmtKind::Item(Box::new(item)),
                    span: self.span_from(lo),
                });
            }
        }

        let kind = if self.check_kw(Keyword::Let) {
            self.advance();
            let (ty, name) = self.parse_typed_binding("variable name");
            self.expect(&TokenKind::Assign);
            let value = self.parse_expr();
            self.expect_stmt_end("let declaration");
            StmtKind::Let(Box::new(LetStmt { ty, name, value }))
        } else if self.check_kw(Keyword::Const) {
            let decl = self.parse_const_decl();
            self.expect_stmt_end("constant declaration");
            StmtKind::Const(Box::new(decl))
        } else if self.check_kw(Keyword::Return) {
            self.advance();
            // `return` alone is valid; a value must start on the same line.
            let value = if self.check(&TokenKind::RBrace)
                || self.check(&TokenKind::Semi)
                || self.at_end()
                || self.newline_before()
            {
                None
            } else {
                Some(Box::new(self.parse_expr()))
            };
            self.expect_stmt_end("return statement");
            StmtKind::Return(value)
        } else if self.check_kw(Keyword::Break) {
            self.advance();
            let label = self.parse_optional_label();
            self.expect_stmt_end("break statement");
            StmtKind::Break(label)
        } else if self.check_kw(Keyword::Continue) {
            self.advance();
            let label = self.parse_optional_label();
            self.expect_stmt_end("continue statement");
            StmtKind::Continue(label)
        } else if self.check_kw(Keyword::Defer) {
            self.advance();
            let block = self.parse_block();
            StmtKind::Defer(Box::new(block))
        } else {
            return Some(self.parse_expr_or_assign_stmt(id, lo));
        };

        Some(Stmt { id, kind, span: self.span_from(lo) })
    }

    /// A loop label after `break`/`continue`, if written on the same line.
    fn parse_optional_label(&mut self) -> Option<Ident> {
        if matches!(self.peek_kind(), TokenKind::Ident(_)) && !self.newline_before() {
            Some(self.parse_ident("loop label"))
        } else {
            None
        }
    }

    /// An expression statement, an assignment, or a labelled loop.
    fn parse_expr_or_assign_stmt(&mut self, id: NodeId, lo: Span) -> Stmt {
        // `outer: for ... { }` (SPEC §22).
        if matches!(self.peek_kind(), TokenKind::Ident(_))
            && matches!(self.peek_at(1).kind, TokenKind::Colon)
            && matches!(
                self.peek_at(2).kind,
                TokenKind::Keyword(Keyword::For)
                    | TokenKind::Keyword(Keyword::While)
                    | TokenKind::Keyword(Keyword::Loop)
            )
        {
            let label = self.parse_ident("loop label");
            self.advance(); // `:`
            let expr = self.parse_labelled_loop(label);
            return Stmt { id, kind: StmtKind::Expr(Box::new(expr)), span: self.span_from(lo) };
        }

        let expr = self.parse_expr();

        // Assignment (SPEC §7).
        if let Some(op) = self.peek_assign_op() {
            let op_span = self.peek().span;
            self.advance();
            let value = self.parse_expr();

            if !expr.is_place() && !expr.is_err() {
                self.error_at(
                    expr.span,
                    Diagnostic::error(E_BAD_ASSIGN_TARGET, "invalid assignment target")
                        .with_primary(expr.span, "cannot assign to this expression")
                        .with_secondary(op_span, "assignment happens here")
                        .with_note(
                            "only variables, fields and index expressions can be assigned to",
                        ),
                );
            }

            self.expect_stmt_end("assignment");
            return Stmt {
                id,
                kind: StmtKind::Assign(Box::new(AssignStmt { target: expr, op, value })),
                span: self.span_from(lo),
            };
        }

        // A block-like expression at statement level needs no terminator, and
        // a trailing expression before `}` is the block's value (SPEC §17).
        let block_like = matches!(
            expr.kind,
            ExprKind::If { .. }
                | ExprKind::Match { .. }
                | ExprKind::For { .. }
                | ExprKind::While { .. }
                | ExprKind::Loop { .. }
                | ExprKind::Block(_)
                | ExprKind::Try { .. }
                | ExprKind::Unsafe(_)
        );

        let is_tail = self.check(&TokenKind::RBrace) && !block_like;

        if !block_like {
            self.expect_stmt_end("expression");
        }

        let kind = if is_tail {
            StmtKind::Tail(Box::new(expr))
        } else {
            StmtKind::Expr(Box::new(expr))
        };
        Stmt { id, kind, span: self.span_from(lo) }
    }

    fn peek_assign_op(&self) -> Option<AssignOp> {
        Some(match self.peek_kind() {
            TokenKind::Assign => AssignOp::Assign,
            TokenKind::PlusEq => AssignOp::Add,
            TokenKind::MinusEq => AssignOp::Sub,
            TokenKind::StarEq => AssignOp::Mul,
            TokenKind::SlashEq => AssignOp::Div,
            TokenKind::PercentEq => AssignOp::Rem,
            _ => return None,
        })
    }

    fn parse_labelled_loop(&mut self, label: Ident) -> Expr {
        let lo = label.span;
        let id = self.next_id();
        let kind = if self.check_kw(Keyword::For) {
            match self.parse_for(Some(label)).kind {
                k => k,
            }
        } else if self.check_kw(Keyword::While) {
            self.parse_while(Some(label)).kind
        } else {
            self.parse_loop(Some(label)).kind
        };
        Expr { id, kind, span: self.span_from(lo) }
    }

    // =======================================================================
    // Expressions
    // =======================================================================

    fn parse_expr(&mut self) -> Expr {
        self.parse_range()
    }

    /// `a..b` and `a..=b` (SPEC §19). Non-associative.
    fn parse_range(&mut self) -> Expr {
        let lo = self.peek().span;

        // An open range `..end`.
        if self.check(&TokenKind::DotDot) || self.check(&TokenKind::DotDotEq) {
            let inclusive = self.check(&TokenKind::DotDotEq);
            self.advance();
            let end = self.parse_coalesce();
            let id = self.next_id();
            return Expr {
                id,
                kind: ExprKind::Range { start: None, end: Some(Box::new(end)), inclusive },
                span: self.span_from(lo),
            };
        }

        let start = self.parse_coalesce();
        if self.check(&TokenKind::DotDot) || self.check(&TokenKind::DotDotEq) {
            let inclusive = self.check(&TokenKind::DotDotEq);
            self.advance();
            // `for i in 0..` with no end is a half-open range.
            let end = if self.starts_expr() {
                Some(Box::new(self.parse_coalesce()))
            } else {
                None
            };
            let id = self.next_id();
            return Expr {
                id,
                kind: ExprKind::Range { start: Some(Box::new(start)), end, inclusive },
                span: self.span_from(lo),
            };
        }
        start
    }

    /// `a ?? b` (SPEC §30). Right-associative, binds looser than `||`.
    fn parse_coalesce(&mut self) -> Expr {
        let lo = self.peek().span;
        let lhs = self.parse_binary(0);
        if self.check(&TokenKind::QuestionQuestion) {
            self.advance();
            let rhs = self.parse_coalesce();
            let id = self.next_id();
            return Expr {
                id,
                kind: ExprKind::Coalesce { lhs: Box::new(lhs), rhs: Box::new(rhs) },
                span: self.span_from(lo),
            };
        }
        lhs
    }

    /// Precedence climbing over the binary operators.
    ///
    /// Levels, loosest first: `||`, `&&`, comparison, `|`, `^`, `&`, shifts,
    /// `+ -`, `* / %`.
    fn parse_binary(&mut self, min_prec: u8) -> Expr {
        let lo = self.peek().span;
        let mut lhs = self.parse_unary();

        while let Some((op, prec)) = self.peek_binop() {
            if prec < min_prec {
                break;
            }
            self.advance();
            // All binary operators in L are left-associative.
            let rhs = self.parse_binary(prec + 1);
            let id = self.next_id();
            lhs = Expr {
                id,
                kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
                span: self.span_from(lo),
            };
        }
        lhs
    }

    fn peek_binop(&self) -> Option<(BinOp, u8)> {
        use TokenKind as T;
        Some(match self.peek_kind() {
            T::OrOr => (BinOp::Or, 0),
            T::AndAnd => (BinOp::And, 1),
            T::EqEq => (BinOp::Eq, 2),
            T::NotEq => (BinOp::Ne, 2),
            T::Lt => (BinOp::Lt, 2),
            T::Le => (BinOp::Le, 2),
            T::Gt => (BinOp::Gt, 2),
            T::Ge => (BinOp::Ge, 2),
            T::Pipe => (BinOp::BitOr, 3),
            T::Caret => (BinOp::BitXor, 4),
            T::Amp => (BinOp::BitAnd, 5),
            T::Shl => (BinOp::Shl, 6),
            T::Shr => (BinOp::Shr, 6),
            T::Plus => (BinOp::Add, 7),
            T::Minus => (BinOp::Sub, 7),
            T::Star => (BinOp::Mul, 8),
            T::Slash => (BinOp::Div, 8),
            T::Percent => (BinOp::Rem, 8),
            _ => return None,
        })
    }

    fn parse_unary(&mut self) -> Expr {
        let lo = self.peek().span;

        let op = match self.peek_kind() {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Bang => Some(UnOp::Not),
            TokenKind::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.parse_unary();
            let id = self.next_id();
            return Expr {
                id,
                kind: ExprKind::Unary { op, operand: Box::new(operand) },
                span: self.span_from(lo),
            };
        }

        // `await expr` (SPEC §68).
        if self.check_kw(Keyword::Await) {
            self.advance();
            let inner = self.parse_unary();
            let id = self.next_id();
            return Expr {
                id,
                kind: ExprKind::Await(Box::new(inner)),
                span: self.span_from(lo),
            };
        }

        // `spawn expr` or `spawn { ... }` (SPEC §68, §69).
        if self.check_kw(Keyword::Spawn) {
            self.advance();
            let inner = if self.check(&TokenKind::LBrace) {
                let block = self.parse_block();
                let id = self.next_id();
                let span = block.span;
                Expr { id, kind: ExprKind::Block(block), span }
            } else {
                self.parse_unary()
            };
            let id = self.next_id();
            return Expr {
                id,
                kind: ExprKind::Spawn(Box::new(inner)),
                span: self.span_from(lo),
            };
        }

        // `call f(args)` (SPEC §2.3).
        if self.check_kw(Keyword::Call) {
            self.advance();
            let inner = self.parse_postfix();
            // Mark the outermost call as having been written with `call`.
            let inner = match inner {
                Expr { id, kind: ExprKind::Call { callee, args, .. }, span } => Expr {
                    id,
                    kind: ExprKind::Call { callee, args, has_call_keyword: true },
                    span,
                },
                other => {
                    self.error_at(
                        other.span,
                        Diagnostic::error(E_MISSING_CALL, "`call` must be followed by a call")
                            .with_primary(other.span, "this is not a function call")
                            .with_note(
                                "`call` marks function invocation, as in `call print(\"hi\")`",
                            ),
                    );
                    other
                }
            };
            return inner;
        }

        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Expr {
        let lo = self.peek().span;
        let mut expr = self.parse_primary();

        loop {
            match self.peek_kind().clone() {
                // `.field`, `.0`, `.method(...)`
                TokenKind::Dot => {
                    self.advance();
                    match self.peek_kind().clone() {
                        TokenKind::Int { value, .. } => {
                            let span = self.peek().span;
                            self.advance();
                            if value > u32::MAX as u128 {
                                self.error_at(
                                    span,
                                    Diagnostic::error(E_BAD_TUPLE_INDEX, "tuple index is too large")
                                        .with_primary(span, "index out of range"),
                                );
                            }
                            let id = self.next_id();
                            expr = Expr {
                                id,
                                kind: ExprKind::TupleField {
                                    base: Box::new(expr),
                                    index: value as u32,
                                    span,
                                },
                                span: self.span_from(lo),
                            };
                        }
                        _ => {
                            let name = self.parse_ident("field or method name");

                            // `users.User { ... }` — a struct literal named by a
                            // qualified path. Only a chain of plain names can be
                            // one, so `f().x { ... }` is never reinterpreted.
                            if self.check(&TokenKind::LBrace) && !self.no_struct_lit {
                                if let Some(mut path) = flatten_path(&expr) {
                                    path.segments.push(name);
                                    path.span = path.span.to(self.prev().span);
                                    return self.parse_struct_lit(lo, path, Vec::new());
                                }
                            }

                            let id = self.next_id();
                            expr = Expr {
                                id,
                                kind: ExprKind::Field { base: Box::new(expr), name },
                                span: self.span_from(lo),
                            };
                        }
                    }
                }

                // `?.field` (SPEC §30)
                TokenKind::QuestionDot => {
                    self.advance();
                    let name = self.parse_ident("field or method name");
                    let id = self.next_id();
                    expr = Expr {
                        id,
                        kind: ExprKind::OptionalField { base: Box::new(expr), name },
                        span: self.span_from(lo),
                    };
                }

                // `[index]`
                TokenKind::LBracket => {
                    self.advance();
                    let saved = std::mem::replace(&mut self.no_struct_lit, false);
                    let index = self.parse_expr();
                    self.no_struct_lit = saved;
                    self.expect(&TokenKind::RBracket);
                    let id = self.next_id();
                    expr = Expr {
                        id,
                        kind: ExprKind::Index { base: Box::new(expr), index: Box::new(index) },
                        span: self.span_from(lo),
                    };
                }

                // `(args)`
                TokenKind::LParen => {
                    self.advance();
                    let saved = std::mem::replace(&mut self.no_struct_lit, false);
                    let mut args = Vec::new();
                    while !self.check(&TokenKind::RParen) && !self.at_end() {
                        args.push(self.parse_expr());
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.no_struct_lit = saved;
                    self.expect(&TokenKind::RParen);
                    let id = self.next_id();
                    expr = Expr {
                        id,
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                            has_call_keyword: false,
                        },
                        span: self.span_from(lo),
                    };
                }

                _ => break,
            }
        }

        expr
    }

    /// Whether the next token can begin an expression.
    fn starts_expr(&self) -> bool {
        use TokenKind as T;
        matches!(
            self.peek_kind(),
            T::Ident(_)
                | T::Int { .. }
                | T::Float { .. }
                | T::Str(_)
                | T::Char(_)
                | T::LParen
                | T::LBracket
                | T::LBrace
                | T::Minus
                | T::Bang
                | T::Tilde
        ) || matches!(
            self.peek_kind(),
            T::Keyword(
                Keyword::True
                    | Keyword::False
                    | Keyword::Null
                    | Keyword::Self_
                    | Keyword::Call
                    | Keyword::If
                    | Keyword::Match
                    | Keyword::Await
                    | Keyword::Spawn
                    | Keyword::Try
                    | Keyword::Unsafe
            )
        )
    }

    fn parse_primary(&mut self) -> Expr {
        let lo = self.peek().span;

        let kind = match self.peek_kind().clone() {
            TokenKind::Int { value, suffix, .. } => {
                self.advance();
                ExprKind::Int { value, suffix }
            }
            TokenKind::Float { value, suffix } => {
                self.advance();
                ExprKind::Float { value, suffix }
            }
            TokenKind::Char(c) => {
                self.advance();
                ExprKind::Char(c)
            }
            TokenKind::Str(parts) => {
                self.advance();
                ExprKind::Str(self.lower_str_parts(parts))
            }
            TokenKind::Keyword(Keyword::True) => {
                self.advance();
                ExprKind::Bool(true)
            }
            TokenKind::Keyword(Keyword::False) => {
                self.advance();
                ExprKind::Bool(false)
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.advance();
                ExprKind::Null
            }
            TokenKind::Keyword(Keyword::Self_) => {
                self.advance();
                ExprKind::SelfExpr
            }

            // Control flow in expression position.
            TokenKind::Keyword(Keyword::If) => return self.parse_if(),
            TokenKind::Keyword(Keyword::Match) => return self.parse_match(),
            TokenKind::Keyword(Keyword::For) => return self.parse_for(None),
            TokenKind::Keyword(Keyword::While) => return self.parse_while(None),
            TokenKind::Keyword(Keyword::Loop) => return self.parse_loop(None),
            TokenKind::Keyword(Keyword::Try) => return self.parse_try(),
            TokenKind::Keyword(Keyword::Unsafe) => {
                self.advance();
                let block = self.parse_block();
                ExprKind::Unsafe(block)
            }

            // `(a)` or `(a, b)` (SPEC §15).
            TokenKind::LParen => {
                self.advance();
                let saved = std::mem::replace(&mut self.no_struct_lit, false);
                let mut items = Vec::new();
                let mut trailing_comma = false;
                while !self.check(&TokenKind::RParen) && !self.at_end() {
                    items.push(self.parse_expr());
                    if self.eat(&TokenKind::Comma) {
                        trailing_comma = true;
                    } else {
                        trailing_comma = false;
                        break;
                    }
                }
                self.no_struct_lit = saved;
                self.expect(&TokenKind::RParen);
                if items.len() == 1 && !trailing_comma {
                    // Parenthesised expression; keep the inner node but widen
                    // its span so diagnostics cover the parentheses.
                    let mut inner = items.pop().expect("checked len");
                    inner.span = self.span_from(lo);
                    return inner;
                }
                ExprKind::Tuple(items)
            }

            // `[1, 2, 3]` (SPEC §12).
            TokenKind::LBracket => {
                self.advance();
                let saved = std::mem::replace(&mut self.no_struct_lit, false);
                let mut items = Vec::new();
                while !self.check(&TokenKind::RBracket) && !self.at_end() {
                    items.push(self.parse_expr());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.no_struct_lit = saved;
                self.expect(&TokenKind::RBracket);
                ExprKind::Array(items)
            }

            // `{ ... }` — a map literal, a set literal, or a block.
            TokenKind::LBrace => return self.parse_brace_expr(),

            TokenKind::Ident(_) => {
                // Only a single segment is taken here. A dotted expression like
                // `user.name` or `math.sqrt` becomes a chain of `Field` nodes in
                // `parse_postfix`; deciding which links are module qualifiers
                // and which are real field accesses needs name resolution, so
                // the parser does not guess.
                let path = Path::single(self.parse_ident("name"));

                // `Box<int> { ... }` or `Box<int>(...)`: try generic args.
                let generics = self.try_parse_generic_args_for_expr();

                // `User { name: ... }` (SPEC §23), unless we are in a loop or
                // conditional head where `{` opens the body.
                if self.check(&TokenKind::LBrace) && !self.no_struct_lit {
                    return self.parse_struct_lit(lo, path, generics);
                }

                ExprKind::Path(path)
            }

            // A C-style `for` is a common mistake worth naming (SPEC §19).
            TokenKind::Semi => {
                let span = self.peek().span;
                self.error_at(
                    span,
                    Diagnostic::error(E_EXPECTED_EXPR, "expected an expression, found `;`")
                        .with_primary(span, "expected an expression"),
                );
                self.advance();
                ExprKind::Err
            }

            other => {
                let span = self.peek().span;
                self.error_at(
                    span,
                    Diagnostic::error(
                        E_EXPECTED_EXPR,
                        format!("expected an expression, found {}", other.describe()),
                    )
                    .with_primary(span, "expected an expression"),
                );
                // Do not consume `}` or `)`; the caller needs them to recover.
                if !matches!(
                    other,
                    TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket | TokenKind::Eof
                ) {
                    self.advance();
                }
                ExprKind::Err
            }
        };

        let id = self.next_id();
        Expr { id, kind, span: self.span_from(lo) }
    }

    /// Convert lexer string parts into AST segments, parsing `${...}` bodies.
    fn lower_str_parts(&mut self, parts: Vec<StrPart>) -> Vec<StrSegment> {
        let mut out = Vec::new();
        for part in parts {
            match part {
                StrPart::Literal(text) => out.push(StrSegment::Literal(text)),
                // `$name` and `$user.name` become a path expression.
                StrPart::Ident { name, span } => {
                    let mut offset = span.lo;
                    let segments: Vec<Ident> = name
                        .split('.')
                        .map(|seg| {
                            let seg_span =
                                Span::new(span.file, offset, offset + seg.len() as BytePos);
                            offset += seg.len() as BytePos + 1; // `.`
                            Ident::new(seg, seg_span)
                        })
                        .collect();

                    // `$name` is a variable; `$self.name` and `$user.name` are
                    // field accesses, matching what the same text means outside
                    // a string (SPEC §11, §27).
                    let head_id = self.next_id();
                    let head = &segments[0];
                    let mut expr = if head.name == "self" {
                        Expr { id: head_id, kind: ExprKind::SelfExpr, span: head.span }
                    } else {
                        Expr {
                            id: head_id,
                            kind: ExprKind::Path(Path::single(head.clone())),
                            span: head.span,
                        }
                    };
                    for seg in &segments[1..] {
                        let fid = self.next_id();
                        let fspan = expr.span.to(seg.span);
                        expr = Expr {
                            id: fid,
                            kind: ExprKind::Field { base: Box::new(expr), name: seg.clone() },
                            span: fspan,
                        };
                    }
                    out.push(StrSegment::Interp(Box::new(expr)));
                }
                // `${expr}` — parse the sub-token stream.
                StrPart::Expr { tokens, span } => {
                    let mut sub = Parser::new(self.file, self.src, tokens);
                    sub.next_id = self.next_id;
                    let expr = sub.parse_expr();
                    self.next_id = sub.next_id;
                    self.diagnostics.extend(sub.diagnostics);
                    let _ = span;
                    out.push(StrSegment::Interp(Box::new(expr)));
                }
            }
        }
        out
    }

    /// Speculatively parse `<...>` as generic arguments in expression position.
    ///
    /// Kept only if followed by `{` or `(`, so `a < b` still parses as a
    /// comparison.
    fn try_parse_generic_args_for_expr(&mut self) -> Vec<Type> {
        if !self.check(&TokenKind::Lt) {
            return Vec::new();
        }
        let start = self.pos;
        let start_ids = self.next_id;
        let start_diags = self.diagnostics.len();

        let args = self.parse_generic_args();
        let ok = !args.is_empty()
            && (self.check(&TokenKind::LBrace) || self.check(&TokenKind::LParen))
            && self.diagnostics.len() == start_diags;

        if ok {
            args
        } else {
            self.pos = start;
            self.next_id = start_ids;
            self.truncate_diagnostics(start_diags);
            Vec::new()
        }
    }

    fn parse_struct_lit(&mut self, lo: Span, path: Path, generics: Vec<Type>) -> Expr {
        self.expect(&TokenKind::LBrace);
        let saved = std::mem::replace(&mut self.no_struct_lit, false);
        let mut fields = Vec::new();

        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            let field_lo = self.peek().span;
            let name = self.parse_ident("field name");
            self.expect(&TokenKind::Colon);
            let value = self.parse_expr();
            let id = self.next_id();
            fields.push(FieldInit { id, name, value, span: self.span_from(field_lo) });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }

        self.no_struct_lit = saved;
        self.expect(&TokenKind::RBrace);
        let id = self.next_id();
        Expr {
            id,
            kind: ExprKind::StructLit { path, generics, fields },
            span: self.span_from(lo),
        }
    }

    /// Disambiguate `{ ... }` between a map literal, a set literal and a block.
    fn parse_brace_expr(&mut self) -> Expr {
        let lo = self.peek().span;

        // Look at what follows `{` without committing.
        let start = self.pos;
        let start_ids = self.next_id;
        let start_diags = self.diagnostics.len();

        self.advance(); // `{`

        // `{}` is an empty block.
        if self.check(&TokenKind::RBrace) {
            self.pos = start;
            self.next_id = start_ids;
            let block = self.parse_block();
            let id = self.next_id();
            let span = block.span;
            return Expr { id, kind: ExprKind::Block(block), span };
        }

        // A statement keyword means this is certainly a block.
        if matches!(
            self.peek_kind(),
            TokenKind::Keyword(
                Keyword::Let
                    | Keyword::Const
                    | Keyword::Return
                    | Keyword::Break
                    | Keyword::Continue
                    | Keyword::Defer
                    | Keyword::For
                    | Keyword::While
                    | Keyword::Loop
                    | Keyword::If
                    | Keyword::Match
                    | Keyword::Fn
                    | Keyword::Struct
                    | Keyword::Enum
            ) | TokenKind::At
        ) {
            self.pos = start;
            self.next_id = start_ids;
            let block = self.parse_block();
            let id = self.next_id();
            let span = block.span;
            return Expr { id, kind: ExprKind::Block(block), span };
        }

        let first = self.parse_expr();
        let clean = self.diagnostics.len() == start_diags;

        // `{ k: v, ... }` is a map (SPEC §13).
        if clean && self.check(&TokenKind::Colon) {
            self.advance();
            let mut pairs = Vec::new();
            let value = self.parse_expr();
            pairs.push((first, value));
            while self.eat(&TokenKind::Comma) {
                if self.check(&TokenKind::RBrace) {
                    break;
                }
                let k = self.parse_expr();
                self.expect(&TokenKind::Colon);
                let v = self.parse_expr();
                pairs.push((k, v));
            }
            self.expect(&TokenKind::RBrace);
            let id = self.next_id();
            return Expr { id, kind: ExprKind::Map(pairs), span: self.span_from(lo) };
        }

        // `{ a, b }` is a set (SPEC §14).
        if clean && self.check(&TokenKind::Comma) {
            let mut items = vec![first];
            while self.eat(&TokenKind::Comma) {
                if self.check(&TokenKind::RBrace) {
                    break;
                }
                items.push(self.parse_expr());
            }
            self.expect(&TokenKind::RBrace);
            let id = self.next_id();
            return Expr { id, kind: ExprKind::Set(items), span: self.span_from(lo) };
        }

        // Anything else: re-parse as a block.
        self.pos = start;
        self.next_id = start_ids;
        self.truncate_diagnostics(start_diags);
        let block = self.parse_block();
        let id = self.next_id();
        let span = block.span;
        Expr { id, kind: ExprKind::Block(block), span }
    }

    /// `if c { } else if c { } else { }` (SPEC §18).
    fn parse_if(&mut self) -> Expr {
        let lo = self.peek().span;
        self.advance(); // `if`

        let saved = std::mem::replace(&mut self.no_struct_lit, true);
        let cond = self.parse_expr();
        self.no_struct_lit = saved;

        let then = self.parse_block();

        let else_branch = if self.eat_kw(Keyword::Else) {
            if self.check_kw(Keyword::If) {
                Some(Box::new(ElseBranch::If(self.parse_if())))
            } else {
                Some(Box::new(ElseBranch::Block(self.parse_block())))
            }
        } else {
            None
        };

        let id = self.next_id();
        Expr {
            id,
            kind: ExprKind::If { cond: Box::new(cond), then, else_branch },
            span: self.span_from(lo),
        }
    }

    /// `match x { Pattern { body } ... }` (SPEC §26).
    fn parse_match(&mut self) -> Expr {
        let lo = self.peek().span;
        self.advance(); // `match`

        let saved = std::mem::replace(&mut self.no_struct_lit, true);
        let scrutinee = self.parse_expr();
        self.no_struct_lit = saved;

        self.expect(&TokenKind::LBrace);
        let mut arms = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            let arm_lo = self.peek().span;
            let pat = self.parse_pattern();
            let body = self.parse_block();
            let id = self.next_id();
            arms.push(MatchArm { id, pat, body, span: self.span_from(arm_lo) });
            self.eat(&TokenKind::Comma);
        }
        let close = self.peek().span;
        self.expect(&TokenKind::RBrace);

        if arms.is_empty() {
            self.error_at(
                close,
                Diagnostic::error(E_EMPTY_MATCH, "`match` has no arms")
                    .with_primary(self.span_from(lo), "this match is empty")
                    .with_note("matches must be exhaustive, so at least one arm is required"),
            );
        }

        let id = self.next_id();
        Expr {
            id,
            kind: ExprKind::Match { scrutinee: Box::new(scrutinee), arms },
            span: self.span_from(lo),
        }
    }

    /// `for x in xs { }` (SPEC §19).
    fn parse_for(&mut self, label: Option<Ident>) -> Expr {
        let lo = label.as_ref().map(|l| l.span).unwrap_or(self.peek().span);
        self.advance(); // `for`

        // Catch `for (int i = 0; ...)`, which the spec calls out (SPEC §19).
        if self.check(&TokenKind::LParen) {
            let span = self.peek().span;
            self.error_at(
                span,
                Diagnostic::error(E_C_STYLE_FOR, "L does not have C-style `for` loops")
                    .with_primary(span, "unexpected `(`")
                    .with_note("L uses `for x in collection` and `for i in 0..10` (SPEC §19)")
                    .with_suggestion("for a counted loop, write", span, "for i in 0..10"),
            );
            self.sync_to_stmt();
            let id = self.next_id();
            return Expr { id, kind: ExprKind::Err, span: self.span_from(lo) };
        }

        let pat = self.parse_pattern();

        if !self.eat_kw(Keyword::In) {
            let span = self.peek().span;
            self.error_at(
                span,
                Diagnostic::error(E_EXPECTED, "expected `in` after the loop variable")
                    .with_primary(span, "expected `in`")
                    .with_note("`for` loops are written `for item in collection`"),
            );
        }

        let saved = std::mem::replace(&mut self.no_struct_lit, true);
        let iter = self.parse_expr();
        self.no_struct_lit = saved;

        let body = self.parse_block();
        let id = self.next_id();
        Expr {
            id,
            kind: ExprKind::For { label, pat, iter: Box::new(iter), body },
            span: self.span_from(lo),
        }
    }

    /// `while c { }` (SPEC §20).
    fn parse_while(&mut self, label: Option<Ident>) -> Expr {
        let lo = label.as_ref().map(|l| l.span).unwrap_or(self.peek().span);
        self.advance(); // `while`

        let saved = std::mem::replace(&mut self.no_struct_lit, true);
        let cond = self.parse_expr();
        self.no_struct_lit = saved;

        let body = self.parse_block();
        let id = self.next_id();
        Expr {
            id,
            kind: ExprKind::While { label, cond: Box::new(cond), body },
            span: self.span_from(lo),
        }
    }

    /// `loop { }` (SPEC §21).
    fn parse_loop(&mut self, label: Option<Ident>) -> Expr {
        let lo = label.as_ref().map(|l| l.span).unwrap_or(self.peek().span);
        self.advance(); // `loop`
        let body = self.parse_block();
        let id = self.next_id();
        Expr { id, kind: ExprKind::Loop { label, body }, span: self.span_from(lo) }
    }

    /// `try { } catch e { }` / `catch FileError e { }` (SPEC §31).
    fn parse_try(&mut self) -> Expr {
        let lo = self.peek().span;
        self.advance(); // `try`
        let body = self.parse_block();

        let mut catches = Vec::new();
        while self.check_kw(Keyword::Catch) {
            let catch_lo = self.peek().span;
            self.advance();

            // `catch { }`, `catch error { }`, or `catch FileError error { }`.
            let (ty, binding) = if self.check(&TokenKind::LBrace) {
                (None, None)
            } else {
                let (ty, name) = self.parse_typed_binding("error binding");
                (ty, Some(name))
            };

            let catch_body = self.parse_block();
            let id = self.next_id();
            catches.push(CatchClause {
                id,
                ty,
                binding,
                body: catch_body,
                span: self.span_from(catch_lo),
            });
        }

        if catches.is_empty() {
            let span = self.span_from(lo);
            self.error_at(
                span,
                Diagnostic::error(E_TRY_NO_CATCH, "`try` block has no `catch`")
                    .with_primary(span, "this `try` handles nothing")
                    .with_note("add a `catch` clause to handle the error (SPEC §31)"),
            );
        }

        let id = self.next_id();
        Expr { id, kind: ExprKind::Try { body, catches }, span: self.span_from(lo) }
    }

    // =======================================================================
    // Patterns (SPEC §19, §26)
    // =======================================================================

    fn parse_pattern(&mut self) -> Pattern {
        let lo = self.peek().span;

        let kind = match self.peek_kind().clone() {
            TokenKind::Underscore => {
                self.advance();
                PatternKind::Wildcard
            }

            // `(a, b)`
            TokenKind::LParen => {
                self.advance();
                let mut items = Vec::new();
                while !self.check(&TokenKind::RParen) && !self.at_end() {
                    items.push(self.parse_pattern());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen);
                PatternKind::Tuple(items)
            }

            TokenKind::Ident(_) => {
                let path = self.parse_path();

                // `Message.TEXT(text)` — a variant with sub-patterns.
                if self.check(&TokenKind::LParen) {
                    self.advance();
                    let mut fields = Vec::new();
                    while !self.check(&TokenKind::RParen) && !self.at_end() {
                        fields.push(self.parse_pattern());
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen);
                    PatternKind::Variant { path, fields }
                } else if path.is_single() {
                    // A bare lowercase-or-any single name binds a new variable.
                    PatternKind::Binding(path.segments.into_iter().next().expect("single"))
                } else {
                    // `Color.RED` — a unit variant.
                    PatternKind::Variant { path, fields: Vec::new() }
                }
            }

            // Literal patterns.
            TokenKind::Int { .. }
            | TokenKind::Float { .. }
            | TokenKind::Str(_)
            | TokenKind::Char(_)
            | TokenKind::Minus
            | TokenKind::Keyword(Keyword::True)
            | TokenKind::Keyword(Keyword::False)
            | TokenKind::Keyword(Keyword::Null) => {
                let expr = self.parse_unary();
                PatternKind::Literal(Box::new(expr))
            }

            other => {
                let span = self.peek().span;
                self.error_at(
                    span,
                    Diagnostic::error(
                        E_EXPECTED_PATTERN,
                        format!("expected a pattern, found {}", other.describe()),
                    )
                    .with_primary(span, "expected a pattern")
                    .with_note("patterns are `_`, a name, a literal, or `Enum.Variant(...)`"),
                );
                if !matches!(other, TokenKind::LBrace | TokenKind::Eof) {
                    self.advance();
                }
                PatternKind::Err
            }
        };

        let id = self.next_id();
        Pattern { id, kind, span: self.span_from(lo) }
    }
}

/// If `expr` is a chain of plain names (`a`, `a.b`, `a.b.c`), return it as a
/// path. Used to recognise qualified struct literals.
fn flatten_path(expr: &Expr) -> Option<Path> {
    match &expr.kind {
        ExprKind::Path(path) => Some(path.clone()),
        ExprKind::Field { base, name } => {
            let mut path = flatten_path(base)?;
            path.segments.push(name.clone());
            path.span = path.span.to(name.span);
            Some(path)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use l_span::FileId;

    fn parse_ok(src: &str) -> SourceUnit {
        let out = parse_source(FileId(0), src);
        let errors: Vec<_> = out
            .diagnostics
            .iter()
            .filter(|d| d.severity == l_span::Severity::Error)
            .map(|d| d.message.clone())
            .collect();
        assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
        out.unit
    }

    fn parse_errs(src: &str) -> Vec<String> {
        parse_source(FileId(0), src)
            .diagnostics
            .iter()
            .filter(|d| d.severity == l_span::Severity::Error)
            .map(|d| d.message.clone())
            .collect()
    }

    fn only_fn(unit: &SourceUnit) -> &FnDecl {
        match &unit.items[0].kind {
            ItemKind::Fn(f) => f,
            other => panic!("expected fn, got {other:?}"),
        }
    }

    fn body_stmts(unit: &SourceUnit) -> &[Stmt] {
        &only_fn(unit).body.as_ref().expect("body").stmts
    }

    /// The expression of a statement, whether or not it is the block's tail.
    ///
    /// A call written last in a block is a tail expression (SPEC §17); whether
    /// its value is used is a question for the type checker, not the parser.
    fn stmt_expr(stmt: &Stmt) -> &Expr {
        match &stmt.kind {
            StmtKind::Expr(e) | StmtKind::Tail(e) => e,
            other => panic!("expected an expression statement, got {other:?}"),
        }
    }

    // ---- items ----

    #[test]
    fn parses_module_and_uses() {
        let unit = parse_ok(
            "module users\n\nuse math\nuse math.sqrt as root\nuse math.sin, math.cos\n",
        );
        assert_eq!(unit.module.as_ref().unwrap().name.name, "users");
        assert_eq!(unit.uses.len(), 3);
        assert_eq!(unit.uses[1].trees[0].alias.as_ref().unwrap().name, "root");
        assert_eq!(unit.uses[2].trees.len(), 2);
        assert_eq!(unit.uses[2].trees[1].path.to_string_dotted(), "math.cos");
    }

    #[test]
    fn rejects_from_import() {
        let errs = parse_errs("use http import Server\n");
        assert!(errs.iter().any(|e| e.contains("from ... import")), "{errs:?}");
    }

    #[test]
    fn parses_a_function_with_types() {
        let unit = parse_ok("fn add(int a, int b) -> int {\n    return a + b\n}\n");
        let f = only_fn(&unit);
        assert_eq!(f.name.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.params[0].ty.render(), "int");
        assert_eq!(f.params[0].name.name, "a");
        assert_eq!(f.ret.as_ref().unwrap().render(), "int");
    }

    #[test]
    fn parses_implicit_return() {
        // §17
        let unit = parse_ok("fn add(int a, int b) -> int {\n    a + b\n}\n");
        let stmts = body_stmts(&unit);
        assert!(matches!(stmts[0].kind, StmtKind::Tail(_)), "{:?}", stmts[0].kind);
    }

    #[test]
    fn parses_methods_with_receivers() {
        // §27
        let unit = parse_ok("fn User.greet() {\n    print(\"hi\")\n}\n");
        let f = only_fn(&unit);
        assert_eq!(f.receiver.as_ref().unwrap().name, "User");
        assert_eq!(f.name.name, "greet");
        assert_eq!(f.qualified_name(), "User.greet");
    }

    #[test]
    fn parses_structs_with_defaults_and_visibility() {
        // §23, §24, §33
        let unit = parse_ok(
            "pub struct User {\n    pub str name\n    int age := 0\n    bool active := true\n}\n",
        );
        assert!(unit.items[0].vis.is_public());
        let ItemKind::Struct(s) = &unit.items[0].kind else { panic!("expected struct") };
        assert_eq!(s.fields.len(), 3);
        assert!(s.fields[0].vis.is_public());
        assert_eq!(s.fields[0].ty.render(), "str");
        assert!(s.fields[1].default.is_some());
        assert!(!s.fields[1].vis.is_public());
    }

    #[test]
    fn parses_enums_with_data_variants() {
        // §25
        let unit = parse_ok("enum Message {\n    TEXT(str)\n    NUMBER(int)\n    QUIT\n}\n");
        let ItemKind::Enum(e) = &unit.items[0].kind else { panic!("expected enum") };
        assert_eq!(e.variants.len(), 3);
        assert!(e.variants[0].has_payload());
        assert_eq!(e.variants[0].payload[0].render(), "str");
        assert!(!e.variants[2].has_payload());
    }

    #[test]
    fn parses_interfaces_and_impls() {
        // §28
        let unit = parse_ok(
            "interface Printable {\n    fn print()\n}\n\nimpl Printable for User {\n    fn print() {\n        call println(self.name)\n    }\n}\n",
        );
        let ItemKind::Interface(i) = &unit.items[0].kind else { panic!("expected interface") };
        assert_eq!(i.methods.len(), 1);
        let ItemKind::Fn(sig) = &i.methods[0].kind else { panic!("expected fn") };
        assert!(sig.body.is_none(), "interface method should have no body");

        let ItemKind::Impl(b) = &unit.items[1].kind else { panic!("expected impl") };
        assert_eq!(b.interface.to_string_dotted(), "Printable");
        assert_eq!(b.target.render(), "User");
        assert_eq!(b.methods.len(), 1);
    }

    #[test]
    fn parses_generics() {
        // §29
        let unit = parse_ok("fn first<T>(T[] items) -> T {\n    return items[0]\n}\n");
        let f = only_fn(&unit);
        assert_eq!(f.generics.len(), 1);
        assert_eq!(f.generics[0].name.name, "T");
        assert_eq!(f.params[0].ty.render(), "T[]");

        let unit = parse_ok("struct Box<T> {\n    T value\n}\n");
        let ItemKind::Struct(s) = &unit.items[0].kind else { panic!("expected struct") };
        assert_eq!(s.generics.len(), 1);
    }

    #[test]
    fn parses_generic_bounds() {
        let unit = parse_ok("fn show<T: Printable>(T item) {\n}\n");
        let f = only_fn(&unit);
        assert_eq!(f.generics[0].bounds[0].to_string_dotted(), "Printable");
    }

    #[test]
    fn parses_consts() {
        // §8
        let unit = parse_ok("const int MAX_USERS := 100\n");
        let ItemKind::Const(c) = &unit.items[0].kind else { panic!("expected const") };
        assert_eq!(c.name.name, "MAX_USERS");
        assert_eq!(c.ty.as_ref().unwrap().render(), "int");
    }

    #[test]
    fn parses_extern_fns() {
        // §72
        let unit = parse_ok("extern fn printf(str format, ...)\n");
        let f = only_fn(&unit);
        assert!(f.is_extern);
        assert!(f.is_variadic);
        assert!(f.body.is_none());
    }

    #[test]
    fn parses_attributes_and_docs() {
        // §6, §73
        let unit = parse_ok(
            "/// Adds two numbers.\n/// Second line.\n@inline\nfn add(int a, int b) -> int {\n    a + b\n}\n",
        );
        let item = &unit.items[0];
        assert_eq!(item.docs.lines.len(), 2);
        assert_eq!(item.docs.summary(), "Adds two numbers. Second line.");
        assert!(item.has_attr("inline"));
    }

    #[test]
    fn parses_test_attribute() {
        // §74
        let unit = parse_ok("@test\nfn test_add() {\n    assert(call add(2, 3) == 5)\n}\n");
        assert!(unit.items[0].is_test());
    }

    #[test]
    fn parses_deprecated_with_argument() {
        let unit = parse_ok("@deprecated(\"Use new_function\")\nfn old_function() {\n}\n");
        let attr = &unit.items[0].attrs[0];
        assert_eq!(attr.name.name, "deprecated");
        assert_eq!(attr.args.len(), 1);
    }

    // ---- statements ----

    #[test]
    fn parses_typed_and_inferred_lets() {
        // §7
        let unit = parse_ok("fn main() {\n    let int age := 20\n    let name := \"Sasha\"\n}\n");
        let stmts = body_stmts(&unit);
        let StmtKind::Let(typed) = &stmts[0].kind else { panic!("expected let") };
        assert_eq!(typed.ty.as_ref().unwrap().render(), "int");
        assert_eq!(typed.name.name, "age");

        let StmtKind::Let(inferred) = &stmts[1].kind else { panic!("expected let") };
        assert!(inferred.ty.is_none());
        assert_eq!(inferred.name.name, "name");
    }

    #[test]
    fn parses_complex_let_types() {
        let unit = parse_ok(
            "fn main() {\n    let int[] numbers := [1, 2]\n    let map<str, int> users := {\"a\": 1}\n    let str? name := null\n    let set<str> names := {\"a\", \"b\"}\n}\n",
        );
        let stmts = body_stmts(&unit);
        let tys: Vec<String> = stmts
            .iter()
            .map(|s| match &s.kind {
                StmtKind::Let(l) => l.ty.as_ref().unwrap().render(),
                other => panic!("expected let, got {other:?}"),
            })
            .collect();
        assert_eq!(tys, vec!["int[]", "map<str, int>", "str?", "set<str>"]);
    }

    #[test]
    fn parses_assignments_and_compound_assignments() {
        // §7
        let unit = parse_ok(
            "fn main() {\n    age := 21\n    age += 1\n    age -= 1\n    age *= 2\n    age /= 2\n    age %= 2\n    numbers[0] := 10\n    user.age := 21\n}\n",
        );
        let stmts = body_stmts(&unit);
        let ops: Vec<AssignOp> = stmts
            .iter()
            .map(|s| match &s.kind {
                StmtKind::Assign(a) => a.op,
                other => panic!("expected assignment, got {other:?}"),
            })
            .collect();
        use AssignOp::*;
        assert_eq!(ops, vec![Assign, Add, Sub, Mul, Div, Rem, Assign, Assign]);
    }

    #[test]
    fn rejects_assignment_to_a_literal() {
        let errs = parse_errs("fn main() {\n    5 := 6\n}\n");
        assert!(errs.iter().any(|e| e.contains("invalid assignment target")), "{errs:?}");
    }

    #[test]
    fn parses_defer() {
        // §32
        let unit = parse_ok(
            "fn main() {\n    let file := call open(\"data.txt\")\n\n    defer {\n        call file.close()\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        assert!(matches!(stmts[1].kind, StmtKind::Defer(_)));
    }

    #[test]
    fn parses_bare_return() {
        let unit = parse_ok("fn main() {\n    return\n}\n");
        let stmts = body_stmts(&unit);
        assert!(matches!(stmts[0].kind, StmtKind::Return(None)));
    }

    #[test]
    fn statements_are_newline_terminated() {
        let errs = parse_errs("fn main() {\n    let a := 1 let b := 2\n}\n");
        assert!(errs.iter().any(|e| e.contains("expected end of")), "{errs:?}");
    }

    #[test]
    fn semicolons_separate_statements_on_one_line() {
        let unit = parse_ok("fn main() {\n    let a := 1; let b := 2\n}\n");
        assert_eq!(body_stmts(&unit).len(), 2);
    }

    // ---- control flow ----

    #[test]
    fn parses_if_else_chains() {
        // §18
        let unit = parse_ok(
            "fn main() {\n    if age >= 18 {\n        print(\"Adult\")\n    } else if age >= 13 {\n        print(\"Teen\")\n    } else {\n        print(\"Child\")\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::If { else_branch, .. } = &e.kind else { panic!("expected if") };
        assert!(matches!(else_branch.as_deref(), Some(ElseBranch::If(_))));
    }

    #[test]
    fn condition_braces_are_not_struct_literals() {
        // `if x { }` must not read `x { }` as a struct literal.
        let unit = parse_ok("fn main() {\n    if running {\n        print(\"y\")\n    }\n}\n");
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::If { cond, .. } = &e.kind else { panic!("expected if") };
        assert!(matches!(cond.kind, ExprKind::Path(_)), "{:?}", cond.kind);
    }

    #[test]
    fn parses_all_for_forms() {
        // §19
        let unit = parse_ok(
            "fn main() {\n    for user in users {\n        print(user.name)\n    }\n    for i in 10 {\n        print(i)\n    }\n    for i in 0..10 {\n        print(i)\n    }\n    for i in 0..=10 {\n        print(i)\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        assert_eq!(stmts.len(), 4);

        let iter_kind = |i: usize| -> &ExprKind {
            let e = stmt_expr(&stmts[i]);
            let ExprKind::For { iter, .. } = &e.kind else { panic!("expected for") };
            &iter.kind
        };
        assert!(matches!(iter_kind(0), ExprKind::Path(_)));
        assert!(matches!(iter_kind(1), ExprKind::Int { value: 10, .. }));
        assert!(matches!(iter_kind(2), ExprKind::Range { inclusive: false, .. }));
        assert!(matches!(iter_kind(3), ExprKind::Range { inclusive: true, .. }));
    }

    #[test]
    fn rejects_c_style_for() {
        // §19 calls this out explicitly.
        let errs = parse_errs("fn main() {\n    for (int i = 0; i < 10; i++) {\n    }\n}\n");
        assert!(errs.iter().any(|e| e.contains("C-style")), "{errs:?}");
    }

    #[test]
    fn parses_while_and_loop() {
        // §20, §21
        let unit = parse_ok(
            "fn main() {\n    while running {\n        call update()\n    }\n    loop {\n        call tick()\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        let w = stmt_expr(&stmts[0]);
        assert!(matches!(w.kind, ExprKind::While { .. }));
        let l = stmt_expr(&stmts[1]);
        assert!(matches!(l.kind, ExprKind::Loop { .. }));
    }

    #[test]
    fn parses_labelled_loops_and_labelled_break() {
        // §22
        let unit = parse_ok(
            "fn main() {\n    outer: for x in 10 {\n        for y in 10 {\n            if condition {\n                break outer\n            }\n        }\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::For { label, body, .. } = &e.kind else { panic!("expected for") };
        assert_eq!(label.as_ref().unwrap().name, "outer");

        // Find the labelled break.
        let StmtKind::Expr(inner) = &body.stmts[0].kind else { panic!("expected expr") };
        let ExprKind::For { body: inner_body, .. } = &inner.kind else { panic!("expected for") };
        let if_expr = stmt_expr(&inner_body.stmts[0]);
        let ExprKind::If { then, .. } = &if_expr.kind else { panic!("expected if") };
        let StmtKind::Break(label) = &then.stmts[0].kind else { panic!("expected break") };
        assert_eq!(label.as_ref().unwrap().name, "outer");
    }

    #[test]
    fn parses_match_with_data_variants_and_wildcard() {
        // §26
        let unit = parse_ok(
            "fn main() {\n    match message {\n        Message.TEXT(text) {\n            print(text)\n        }\n\n        Message.QUIT {\n            call quit()\n        }\n\n        _ {\n            print(\"other\")\n        }\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::Match { arms, .. } = &e.kind else { panic!("expected match") };
        assert_eq!(arms.len(), 3);

        let PatternKind::Variant { path, fields } = &arms[0].pat.kind else {
            panic!("expected variant pattern")
        };
        assert_eq!(path.to_string_dotted(), "Message.TEXT");
        assert_eq!(fields.len(), 1);
        assert_eq!(arms[0].pat.bindings()[0].name, "text");

        let PatternKind::Variant { fields, .. } = &arms[1].pat.kind else {
            panic!("expected unit variant pattern")
        };
        assert!(fields.is_empty());

        assert!(matches!(arms[2].pat.kind, PatternKind::Wildcard));
    }

    #[test]
    fn parses_try_catch() {
        // §31
        let unit = parse_ok(
            "fn main() {\n    try {\n        call process()\n    } catch FileError error {\n        print(\"File error\")\n    } catch NetworkError error {\n        print(\"Network error\")\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::Try { catches, .. } = &e.kind else { panic!("expected try") };
        assert_eq!(catches.len(), 2);
        assert_eq!(catches[0].ty.as_ref().unwrap().render(), "FileError");
        assert_eq!(catches[0].binding.as_ref().unwrap().name, "error");
    }

    #[test]
    fn parses_untyped_catch() {
        let unit = parse_ok(
            "fn main() {\n    try {\n        let data := call read_file(\"t.txt\")\n    } catch error {\n        print(\"Error: $error\")\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::Try { catches, .. } = &e.kind else { panic!("expected try") };
        assert!(catches[0].ty.is_none());
        assert_eq!(catches[0].binding.as_ref().unwrap().name, "error");
    }

    // ---- expressions ----

    #[test]
    fn parses_call_keyword() {
        // §2.3
        let unit = parse_ok("fn main() {\n    call print(\"Hello\")\n    let r := call add(5, 10)\n}\n");
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::Call { has_call_keyword, args, .. } = &e.kind else { panic!("expected call") };
        assert!(has_call_keyword);
        assert_eq!(args.len(), 1);

        let StmtKind::Let(l) = &stmts[1].kind else { panic!("expected let") };
        let ExprKind::Call { has_call_keyword, .. } = &l.value.kind else { panic!("expected call") };
        assert!(has_call_keyword);
    }

    #[test]
    fn parses_calls_without_the_keyword() {
        // `print(...)` appears bare throughout the spec's examples.
        let unit = parse_ok("fn main() {\n    print(\"Hello\")\n}\n");
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::Call { has_call_keyword, .. } = &e.kind else { panic!("expected call") };
        assert!(!has_call_keyword);
    }

    #[test]
    fn rejects_call_on_a_non_call() {
        let errs = parse_errs("fn main() {\n    call x\n}\n");
        assert!(errs.iter().any(|e| e.contains("must be followed by a call")), "{errs:?}");
    }

    #[test]
    fn parses_operator_precedence() {
        let unit = parse_ok("fn main() {\n    let x := 1 + 2 * 3\n}\n");
        let stmts = body_stmts(&unit);
        let StmtKind::Let(l) = &stmts[0].kind else { panic!("expected let") };
        let ExprKind::Binary { op, rhs, .. } = &l.value.kind else { panic!("expected binary") };
        assert_eq!(*op, BinOp::Add);
        let ExprKind::Binary { op: inner, .. } = &rhs.kind else { panic!("expected binary") };
        assert_eq!(*inner, BinOp::Mul);
    }

    #[test]
    fn comparison_binds_tighter_than_logical() {
        let unit = parse_ok("fn main() {\n    let x := a < b && c > d\n}\n");
        let stmts = body_stmts(&unit);
        let StmtKind::Let(l) = &stmts[0].kind else { panic!("expected let") };
        let ExprKind::Binary { op, .. } = &l.value.kind else { panic!("expected binary") };
        assert_eq!(*op, BinOp::And);
    }

    #[test]
    fn parses_optional_operators() {
        // §30
        let unit = parse_ok(
            "fn main() {\n    let length := name?.length\n    let actual := name ?? \"Unknown\"\n}\n",
        );
        let stmts = body_stmts(&unit);
        let StmtKind::Let(a) = &stmts[0].kind else { panic!("expected let") };
        assert!(matches!(a.value.kind, ExprKind::OptionalField { .. }));
        let StmtKind::Let(b) = &stmts[1].kind else { panic!("expected let") };
        assert!(matches!(b.value.kind, ExprKind::Coalesce { .. }));
    }

    #[test]
    fn parses_collections() {
        // §12, §13, §14, §15
        let unit = parse_ok(
            "fn main() {\n    let numbers := [1, 2, 3, 4]\n    let users := {\"alice\": 20, \"bob\": 25}\n    let names := {\"Alice\", \"Bob\"}\n    let point := (10, 20)\n}\n",
        );
        let stmts = body_stmts(&unit);
        let value = |i: usize| -> &ExprKind {
            let StmtKind::Let(l) = &stmts[i].kind else { panic!("expected let") };
            &l.value.kind
        };
        assert!(matches!(value(0), ExprKind::Array(v) if v.len() == 4));
        assert!(matches!(value(1), ExprKind::Map(v) if v.len() == 2));
        assert!(matches!(value(2), ExprKind::Set(v) if v.len() == 2));
        assert!(matches!(value(3), ExprKind::Tuple(v) if v.len() == 2));
    }

    #[test]
    fn parses_indexing_and_tuple_access() {
        let unit = parse_ok(
            "fn main() {\n    let first := numbers[0]\n    let x := point.0\n    let y := point.1\n    let n := numbers.length\n}\n",
        );
        let stmts = body_stmts(&unit);
        let value = |i: usize| -> &ExprKind {
            let StmtKind::Let(l) = &stmts[i].kind else { panic!("expected let") };
            &l.value.kind
        };
        assert!(matches!(value(0), ExprKind::Index { .. }));
        assert!(matches!(value(1), ExprKind::TupleField { index: 0, .. }));
        assert!(matches!(value(2), ExprKind::TupleField { index: 1, .. }));
        assert!(matches!(value(3), ExprKind::Field { .. }));
    }

    #[test]
    fn parses_struct_literals() {
        // §23
        let unit = parse_ok(
            "fn main() {\n    let User user := User {\n        name: \"Sasha\",\n        age: 20\n    }\n}\n",
        );
        let stmts = body_stmts(&unit);
        let StmtKind::Let(l) = &stmts[0].kind else { panic!("expected let") };
        let ExprKind::StructLit { path, fields, .. } = &l.value.kind else {
            panic!("expected struct literal")
        };
        assert_eq!(path.to_string_dotted(), "User");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name.name, "name");
    }

    #[test]
    fn parses_generic_struct_literals() {
        // §29: `Box<int> { value: 42 }`
        let unit = parse_ok("fn main() {\n    let Box<int> box := Box<int> {\n        value: 42\n    }\n}\n");
        let stmts = body_stmts(&unit);
        let StmtKind::Let(l) = &stmts[0].kind else { panic!("expected let") };
        let ExprKind::StructLit { generics, .. } = &l.value.kind else {
            panic!("expected struct literal")
        };
        assert_eq!(generics.len(), 1);
        assert_eq!(generics[0].render(), "int");
    }

    #[test]
    fn less_than_is_not_a_generic_argument() {
        let unit = parse_ok("fn main() {\n    let x := a < b\n}\n");
        let stmts = body_stmts(&unit);
        let StmtKind::Let(l) = &stmts[0].kind else { panic!("expected let") };
        let ExprKind::Binary { op, .. } = &l.value.kind else { panic!("expected comparison") };
        assert_eq!(*op, BinOp::Lt);
    }

    #[test]
    fn parses_string_interpolation() {
        // §11
        let unit = parse_ok(
            "fn main() {\n    print(\"Hello, $name\")\n    print(\"Age: ${age + 1}\")\n}\n",
        );
        let stmts = body_stmts(&unit);

        let segments = |i: usize| -> &Vec<StrSegment> {
            let e = stmt_expr(&stmts[i]);
            let ExprKind::Call { args, .. } = &e.kind else { panic!("expected call") };
            let ExprKind::Str(segs) = &args[0].kind else { panic!("expected string") };
            segs
        };

        let simple = segments(0);
        assert_eq!(simple.len(), 2);
        assert!(matches!(&simple[0], StrSegment::Literal(t) if t == "Hello, "));
        let StrSegment::Interp(e) = &simple[1] else { panic!("expected interpolation") };
        assert!(matches!(e.kind, ExprKind::Path(_)));

        let complex = segments(1);
        let StrSegment::Interp(e) = &complex[1] else { panic!("expected interpolation") };
        assert!(matches!(e.kind, ExprKind::Binary { op: BinOp::Add, .. }));
    }

    #[test]
    fn self_interpolation_is_field_access() {
        // §27: "Hello, $self.name"
        let unit = parse_ok("fn User.greet() {\n    print(\"Hello, $self.name\")\n}\n");
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        let ExprKind::Call { args, .. } = &e.kind else { panic!("expected call") };
        let ExprKind::Str(segs) = &args[0].kind else { panic!("expected string") };
        let StrSegment::Interp(inner) = &segs[1] else { panic!("expected interpolation") };
        let ExprKind::Field { base, name } = &inner.kind else { panic!("expected field access") };
        assert!(matches!(base.kind, ExprKind::SelfExpr));
        assert_eq!(name.name, "name");
    }

    #[test]
    fn parses_async_await_and_spawn() {
        // §68, §69
        let unit = parse_ok(
            "async fn download(str url) -> str {\n    let response := await call http.get(url)\n    return response.body\n}\n\nfn main() {\n    let task := spawn download(url)\n    let data := await task\n    let t2 := spawn {\n        call process()\n    }\n}\n",
        );
        let f = only_fn(&unit);
        assert!(f.is_async);

        let ItemKind::Fn(main) = &unit.items[1].kind else { panic!("expected fn") };
        let stmts = &main.body.as_ref().unwrap().stmts;
        let StmtKind::Let(a) = &stmts[0].kind else { panic!("expected let") };
        assert!(matches!(a.value.kind, ExprKind::Spawn(_)));
        let StmtKind::Let(b) = &stmts[1].kind else { panic!("expected let") };
        assert!(matches!(b.value.kind, ExprKind::Await(_)));
        let StmtKind::Let(c) = &stmts[2].kind else { panic!("expected let") };
        let ExprKind::Spawn(inner) = &c.value.kind else { panic!("expected spawn") };
        assert!(matches!(inner.kind, ExprKind::Block(_)));
    }

    #[test]
    fn parses_unsafe_blocks() {
        // §71
        let unit = parse_ok("fn main() {\n    unsafe {\n        call raw()\n    }\n}\n");
        let stmts = body_stmts(&unit);
        let e = stmt_expr(&stmts[0]);
        assert!(matches!(e.kind, ExprKind::Unsafe(_)));
    }

    #[test]
    fn parses_channels() {
        // §69
        let unit = parse_ok(
            "fn main() {\n    let channel := call channel<int>(10)\n    call channel.send(42)\n    let value := await channel.receive()\n}\n",
        );
        assert_eq!(body_stmts(&unit).len(), 3);
    }

    // ---- recovery ----

    #[test]
    fn recovers_and_reports_multiple_errors() {
        let errs = parse_errs("fn a( {\n}\n\nfn b() {\n    let := \n}\n\nfn c() {\n}\n");
        assert!(errs.len() >= 2, "expected several errors, got {errs:?}");
    }

    #[test]
    fn keeps_parsing_items_after_a_bad_one() {
        let out = parse_source(FileId(0), "fn a() {\n    let := 1\n}\n\nfn b() {\n}\n");
        assert_eq!(out.unit.items.len(), 2);
        let ItemKind::Fn(b) = &out.unit.items[1].kind else { panic!("expected fn") };
        assert_eq!(b.name.name, "b");
    }

    #[test]
    fn does_not_hang_on_unclosed_delimiters() {
        // Each of these once risked an infinite loop; they must terminate.
        for src in ["fn a() {", "fn a() { let x := (", "struct S {", "match x {", "fn a() { [ }"] {
            let _ = parse_source(FileId(0), src);
        }
    }

    // ---- whole programs from the specification ----

    #[test]
    fn parses_spec_85_example_application() {
        let unit = parse_ok(
            r#"use http
use json

struct User {
    str name
    int age
}

fn User.greet() {
    print("Hello, $self.name!")
}

fn create_user(str name, int age) -> User {
    return User {
        name: name,
        age: age
    }
}

fn main() {
    let str name := "Sasha"
    let int times := 7

    let User user := call create_user(name, 20)

    call user.greet()

    for i in times {
        print("Iteration: $i")
    }
}
"#,
        );
        assert_eq!(unit.uses.len(), 2);
        assert_eq!(unit.items.len(), 4);
    }

    #[test]
    fn parses_spec_107_core_example() {
        let unit = parse_ok(
            r#"use http

struct User {
    str name
    int age
}

fn greet(User user) {
    print("Hello, $user.name!")
}

fn main() {
    let str name := "Sasha"
    let int times := 7

    let User user := User {
        name: name,
        age: 20
    }

    for i in times {
        call greet(user)
    }
}
"#,
        );
        assert_eq!(unit.items.len(), 3);
    }

    #[test]
    fn parses_spec_35_local_module() {
        let users = parse_ok(
            r#"module users

pub struct User {
    pub str name
    pub int age
}

pub fn create(str name, int age) -> User {
    return User {
        name: name,
        age: age
    }
}
"#,
        );
        assert_eq!(users.module.unwrap().name.name, "users");
        assert!(users.items.iter().all(|i| i.vis.is_public()));

        let main = parse_ok(
            "use users\n\nfn main() {\n    let user := call users.create(\"Sasha\", 20)\n\n    print(user.name)\n}\n",
        );
        assert_eq!(main.uses.len(), 1);
    }

    #[test]
    fn parses_nested_generics() {
        let unit = parse_ok("fn f(Box<Box<int>> b) {\n}\n");
        let f = only_fn(&unit);
        assert_eq!(f.params[0].ty.render(), "Box<Box<int>>");
    }
}
