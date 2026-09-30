; codegraph tags for Python (see go/tags.scm for capture conventions).

; ---- definitions ----------------------------------------------------------
(class_definition
  name: (identifier) @name) @definition.class

(function_definition
  name: (identifier) @name) @definition.function

(module
  (expression_statement
    (assignment
      left: (identifier) @name) @definition.const))

; ---- references -----------------------------------------------------------
(call
  function: (identifier) @name) @reference.call

(call
  function: (attribute
    object: (_) @qualifier
    attribute: (identifier) @name)) @reference.call

(class_definition
  superclasses: (argument_list
    (identifier) @name)) @reference.inherit

(class_definition
  superclasses: (argument_list
    (attribute
      object: (_) @qualifier
      attribute: (identifier) @name))) @reference.inherit

; `class B(Base[T])`, `class C(Generic[T], mod.Base[int])`: subscripted bases.
(class_definition
  superclasses: (argument_list
    (subscript
      value: (identifier) @name))) @reference.inherit

(class_definition
  superclasses: (argument_list
    (subscript
      value: (attribute
        object: (_) @qualifier
        attribute: (identifier) @name)))) @reference.inherit

(type
  (identifier) @name) @reference.type

; callables passed as values
(argument_list
  (attribute
    object: (_) @qualifier
    attribute: (identifier) @name) @reference.value)

(argument_list
  (identifier) @name @reference.value)

; names used as values elsewhere (`cls = Option`, `callback=show_version`,
; `{"type": EngineType}`, `[Class1, Class2]`, `return decorator`, `@pass_context`) and as
; the object of an attribute (`ParameterSource.ENVIRONMENT`, `@cli.command()`). Only names
; defined or imported in the file survive extraction; locals never do.
(attribute
  object: (identifier) @name) @reference.value

(assignment
  right: (identifier) @name) @reference.value

(keyword_argument
  value: (identifier) @name) @reference.value

(pair
  value: (identifier) @name) @reference.value

(list
  (identifier) @name) @reference.value

(tuple
  (identifier) @name) @reference.value

(set
  (identifier) @name) @reference.value

(return_statement
  (identifier) @name) @reference.value

(decorator
  (identifier) @name) @reference.value

; annotation shapes beyond a bare name: `Context | None`, `CompletionItem[str]`
(type
  (binary_operator
    left: (identifier) @name)) @reference.type

(type
  (binary_operator
    right: (identifier) @name)) @reference.type

(type
  (binary_operator
    left: (binary_operator
      left: (identifier) @name))) @reference.type

(type
  (binary_operator
    left: (binary_operator
      right: (identifier) @name))) @reference.type

(type
  (subscript
    value: (identifier) @name)) @reference.type

(type
  (generic_type
    (identifier) @name)) @reference.type

; ---- imports --------------------------------------------------------------
(import_statement) @import
(import_from_statement) @import

; names `from m import a, b as c` binds are references to the imported item
(import_from_statement
  name: (dotted_name . (identifier) @name .)) @reference.binding

(import_from_statement
  name: (aliased_import
    name: (dotted_name . (identifier) @name .))) @reference.binding

; attribute reads (`@click.pass_context`, `click.utils._LazyFile`, `result.output`): kept
; by extract.rs only on an imported module/name or a typed receiver
(attribute
  object: (_) @qualifier
  attribute: (identifier) @name) @reference.value

; `except NoSuchOption:`, `except ParseError as exc:`, `x is Cls`, `lambda: f`,
; `ProgressBar[int]`, `type[ShellComplete]`
(except_clause
  (identifier) @name) @reference.value

(except_clause
  (as_pattern
    .
    (identifier) @name)) @reference.value

(comparison_operator
  (identifier) @name) @reference.value

(lambda
  body: (identifier) @name) @reference.value

(subscript
  value: (identifier) @name) @reference.value

(subscript
  subscript: (identifier) @name) @reference.value