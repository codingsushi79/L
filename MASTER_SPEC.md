LSharp 1.0 Specification
Language: LSharp
File extension: .lsh
Compiler: lsc
Package manager: lpm
Formatter: lformat
Linter: llint
Language server: llsp
Documentation tool: ldoc
Official package registry: LSharp Registry
Manifest: lsharp.toml
Lockfile: lsharp.lock

1. Language Overview
LSharp is a general-purpose, statically typed, compiled programming language designed to be:
simple to learn
readable
expressive
fast to compile
performant at runtime
cross-platform
suitable for applications and systems software
easy to distribute
easy to package
easy to build and test
LSharp should avoid unnecessary syntax while still providing the features expected from a modern general-purpose language.
The language should be capable of building:
command-line applications
desktop applications
web servers
web applications
APIs
games
networking software
databases and database tools
system utilities
embedded software
libraries
developer tools
compilers
operating-system components
high-performance applications

2. Core Philosophy
LSharp follows several principles.
2.1 Explicit but simple
LSharp should make important behavior obvious without requiring excessive syntax.
Example:
let str name := "Sasha"
let int age := 20

rather than requiring verbose declarations.

2.2 use instead of import
LSharp deliberately uses:
use http

rather than:
import http

LSharp does not use Python-style:
from http import Server

Specific symbols are written:
use http.Server


2.3 call for function invocation
LSharp explicitly supports:
call print("Hello")

and:
let result := call add(5, 10)

This makes function invocation visually distinct from ordinary expressions.

2.4 One official toolchain
LSharp is distributed as a unified toolchain.
The official installation provides:
lsc
lpm
lformat
llint
llsp
ldoc

The package manager is not an external community project.
lpm is an official component of LSharp.

3. Toolchain
The official toolchain consists of:
lsc      LSharp compiler
lpm      LSharp Package Manager
lformat  LSharp formatter
llint    LSharp linter
llsp     LSharp language server
ldoc     LSharp documentation generator

A standard installation should provide all of them.

4. Compiler
The compiler executable is:
lsc

Examples:
lsc main.lsh

lsc build

lsc run

For projects, lpm is the preferred interface:
lpm build
lpm run


5. Source Files
LSharp source files use:
.lsh

Example:
main.lsh
server.lsh
database.lsh
users.lsh

Source files are UTF-8 encoded.

6. Comments
Single-line:
// comment

Multiline:
/*
    comment
*/

Documentation:
/// Adds two numbers.
fn add(int a, int b) -> int {
    return a + b
}


7. Variables
Mutable variables use let:
let int age := 20

Type inference:
let age := 20

Assignment:
age := 21

Compound assignment:
age += 1
age -= 1
age *= 2
age /= 2
age %= 2


8. Constants
Constants use const:
const int MAX_USERS := 100

Constants cannot be reassigned.
The compiler should evaluate constant expressions at compile time whenever possible.

9. Primitive Types
LSharp 1.0 defines:
bool

char
str

byte

int
int8
int16
int32
int64
int128

uint
uint8
uint16
uint32
uint64
uint128

float
float32
float64

int and uint are platform-native integer types.

10. Boolean
let bool enabled := true
let bool finished := false

Operators:
&&
||
!

LSharp does not implicitly convert integers to booleans.

11. Strings
let str name := "Sasha"

Characters:
let char first := 'S'

String interpolation:
print("Hello, $name")

Expressions:
print("Age: ${age + 1}")

Standard escapes include:
\n
\r
\t
\\
\"
\'
\0

Strings are UTF-8.

12. Arrays
let int[] numbers := [1, 2, 3, 4]

Type inference:
let numbers := [1, 2, 3, 4]

Indexing:
let first := numbers[0]

Modification:
numbers[0] := 10

Length:
numbers.length


13. Maps
let map<str, int> users := {
    "alice": 20,
    "bob": 25
}

Access:
let age := users["alice"]

Modification:
users["alice"] := 21


14. Sets
let set<str> names := {
    "Alice",
    "Bob",
    "Charlie"
}

Operations:
call names.add("Sasha")
call names.remove("Bob")


15. Tuples
let point := (10, 20)

Access:
let x := point.0
let y := point.1


16. Functions
Basic function:
fn add(int a, int b) -> int {
    return a + b
}

Calling:
let result := call add(5, 10)

Functions without a return type return void unless a return expression is present.

17. Expression Returns
The final expression may be returned implicitly:
fn add(int a, int b) -> int {
    a + b
}

Explicit return remains available:
fn add(int a, int b) -> int {
    return a + b
}


18. If Statements
if age >= 18 {
    print("Adult")
}

if age >= 18 {
    print("Adult")
} else {
    print("Minor")
}

if age >= 18 {
    print("Adult")
} else if age >= 13 {
    print("Teen")
} else {
    print("Child")
}


19. For Loops
LSharp uses a unified for ... in model.
Iterating a collection:
for user in users {
    print(user.name)
}

Repeating a number of times:
for i in 10 {
    print(i)
}

This produces:
0
1
2
...
9

Ranges:
for i in 0..10 {
    print(i)
}

Inclusive ranges:
for i in 0..=10 {
    print(i)
}

This keeps loops distinct from C-style:
for (int i = 0; ...)

while remaining easy to understand.

20. While Loops
while running {
    call update()
}


21. Infinite Loops
loop {
    call tick()
}


22. Loop Control
break

continue

Labeled loops:
outer: for x in 10 {
    for y in 10 {
        if condition {
            break outer
        }
    }
}


23. Structs
struct User {
    str name
    int age
}

Creation:
let User user := User {
    name: "Sasha",
    age: 20
}

Access:
print(user.name)

Modification:
user.age := 21


24. Struct Defaults
struct User {
    str name
    int age := 0
    bool active := true
}


25. Enums
enum Color {
    RED
    GREEN
    BLUE
}

Usage:
let Color color := Color.RED

Data variants:
enum Message {
    TEXT(str)
    NUMBER(int)
    QUIT
}


26. Match
match message {
    Message.TEXT(text) {
        print(text)
    }

    Message.NUMBER(number) {
        print(number)
    }

    Message.QUIT {
        call quit()
    }
}

Wildcard:
match color {
    Color.RED {
        print("red")
    }

    _ {
        print("other")
    }
}

Matches should be exhaustive.

27. Methods
Methods use:
fn User.greet() {
    print("Hello, $self.name")
}

Call:
call user.greet()

self refers to the current instance.

28. Interfaces
interface Printable {
    fn print()
}

Implementation:
impl Printable for User {
    fn print() {
        call println(self.name)
    }
}


29. Generics
Functions:
fn first<T>(T[] items) -> T {
    return items[0]
}

Structs:
struct Box<T> {
    T value
}

Usage:
let Box<int> box := Box<int> {
    value: 42
}


30. Optional Types
let str? name := null

Optional access:
let length := name?.length

Fallback:
let actual := name ?? "Unknown"

Only nullable types may contain null.

31. Error Handling
try {
    let data := call read_file("test.txt")
} catch error {
    print("Error: $error")
}

Typed catches:
try {
    call process()
} catch FileError error {
    print("File error")
} catch NetworkError error {
    print("Network error")
}


32. Defer
let file := call open("data.txt")

defer {
    call file.close()
}

Deferred code runs when the current scope exits.

33. Modules
Files can define modules:
module users

A module can expose public declarations:
pub struct User {
    pub str name
    int age
}

Private declarations are the default.

34. use
LSharp uses use for module and package access.
Entire module:
use math

Specific symbol:
use math.sqrt

Multiple:
use math.sqrt, math.sin, math.cos

Alias:
use database as db

Specific alias:
use math.sqrt as root

LSharp does not define Python-style from ... import ... syntax.

35. Local Modules
Project:
src/
├── main.lsh
├── users.lsh
└── database.lsh

users.lsh:
module users

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

main.lsh:
use users

fn main() {
    let user := call users.create("Sasha", 20)

    print(user.name)
}


36. Package System
LSharp packages are first-class projects.
Every package contains:
lsharp.toml
src/

Example:
myapp/
├── lsharp.toml
├── lsharp.lock
├── src/
│   └── main.lsh
├── tests/
└── README.md


37. Package Manifest
Example:
[package]
name = "myapp"
version = "1.0.0"
description = "My LSharp application"
license = "MIT"
authors = ["Sasha"]

[dependencies]
http = "^1.2.0"
json = "^2.0.0"

[dev-dependencies]
test = "^1.0.0"


38. Official LPM
LPM is part of the official LSharp distribution.
It is not a third-party tool.
Installation of LSharp provides:
lsc
lpm
lformat
llint
llsp
ldoc

The official LSharp distribution MUST keep compiler and package-manager compatibility synchronized.

39. LPM Commands
Create:
lpm new myapp

Create a library:
lpm new --lib mylib

Install a dependency:
lpm add http

Install a specific version:
lpm add http@1.2.0

Remove:
lpm remove http

Install dependencies:
lpm install

Update:
lpm update

Build:
lpm build

Run:
lpm run

Test:
lpm test

Search:
lpm search http

Package information:
lpm info http

Publish:
lpm publish

Login:
lpm login

Logout:
lpm logout


40. Lockfile
lpm install generates:
lsharp.lock

The lockfile contains exact resolved dependency versions.
Example:
[[package]]
name = "http"
version = "1.2.3"
source = "registry"
checksum = "..."

[[package]]
name = "json"
version = "2.1.0"
source = "registry"
checksum = "..."

Applications should commit lsharp.lock.
Libraries may also commit it, although dependency resolution rules determine how it is consumed downstream.

41. Dependency Sources
LPM 1.0 supports:
registry
git
path

Registry:
[dependencies]
http = "1.2.0"

Git:
[dependencies]
http = {
    git = "https://github.com/example/http"
}

Git branch:
[dependencies]
http = {
    git = "https://github.com/example/http",
    branch = "development"
}

Git tag:
[dependencies]
http = {
    git = "https://github.com/example/http",
    tag = "v1.2.0"
}

Local:
[dependencies]
http = {
    path = "../http"
}


42. Semantic Versioning
LSharp packages use Semantic Versioning:
MAJOR.MINOR.PATCH

Examples:
1.0.0
1.4.2
2.0.0

Dependency ranges:
http = "^1.4.0"

allows compatible 1.x releases.
http = "~1.4.0"

allows compatible 1.4.x releases.
Exact:
http = "=1.4.0"


43. Official LSharp Registry
The official package registry is a central service operated as part of the LSharp ecosystem.
Conceptually:
registry.lsharp.dev

The registry provides:
package hosting
package metadata
version management
checksums
downloads
dependency metadata
package search
documentation
ownership
publishing
authentication
yanking
security reporting
The registry should provide a web interface similar in purpose to major language package registries.

44. Registry Package URLs
A package should have a canonical page:
registry.lsharp.dev/packages/http

A specific version:
registry.lsharp.dev/packages/http/1.2.0

Documentation:
registry.lsharp.dev/packages/http/1.2.0/docs

The exact domain is implementation-dependent until the official registry is deployed.

45. Package Names
Package names MUST be globally unique within the official registry.
Names:
http
json
sqlite
web
crypto

are valid.
Package names should:
use lowercase
contain letters
contain numbers
allow - where appropriate
not begin with a number
The registry reserves official namespaces where necessary.

46. Publishing Packages
A developer logs into the registry:
lpm login

Then:
lpm publish

LPM validates:
package name
version
manifest
source tree
dependencies
license
README
build
tests

The package is uploaded to the registry.

47. Publishing Requirements
A package release MUST contain:
lsharp.toml
source
version
package name

The registry SHOULD strongly recommend:
README.md
LICENSE
tests/
documentation
repository URL

Packages should be built and tested before publishing.
lpm publish should perform validation automatically.

48. Package Archives
Published packages are immutable releases.
A package version such as:
http@1.2.0

cannot be silently replaced.
The registry stores a cryptographic checksum.
This prevents:
http@1.2.0

from changing underneath existing projects.

49. Package Yanking
A broken or malicious package version may be yanked.
Example:
lpm yank http@1.2.0

Yanking does not delete the package from existing lockfiles.
Instead, the registry marks the version:
YANKED

New projects should receive a warning or avoid the release depending on dependency constraints.

50. GitHub Publishing
The LSharp registry MUST eventually support direct GitHub-based publishing.
A package repository can contain:
.github/
└── workflows/
    └── publish.yml

A GitHub Action can build and publish the package automatically.
Example workflow concept:
name: Publish

on:
  release:
    types: [published]

jobs:
  publish:
    runs-on: ubuntu-latest

    permissions:
      id-token: write
      contents: read

    steps:
      - uses: actions/checkout@v4

      - uses: lsharp/setup-lsharp@v1

      - run: lpm publish

The exact action name and authentication mechanism will be finalized with the registry implementation.

51. GitHub Repository Linking
Packages should be linkable to GitHub repositories.
Manifest:
[package]
name = "http"
version = "1.2.0"
repository = "https://github.com/lsharp/http"

The registry can display:
Source
Issues
Pull Requests
Releases
Documentation

and link back to GitHub.

52. GitHub Release Publishing
The preferred automated publishing workflow should be:
Developer
   ↓
git tag v1.2.0
   ↓
GitHub Release
   ↓
GitHub Actions
   ↓
LSharp build + test
   ↓
lpm publish
   ↓
LSharp Registry

This allows packages to use GitHub as their source-control and release platform while the LSharp Registry acts as the package distribution system.

53. Trusted GitHub Publishing
The registry SHOULD support short-lived credentials rather than requiring developers to put permanent registry passwords into GitHub secrets.
Preferred model:
GitHub Actions
       ↓
OIDC identity
       ↓
LSharp Registry
       ↓
verify repository
       ↓
verify package ownership
       ↓
publish

This minimizes long-lived publishing secrets.

54. Package Ownership
A package should have one or more owners.
Owners can:
publish releases
yank releases
transfer ownership
manage maintainers
configure GitHub publishing

Example:
http
├── owner: lsharp
├── maintainer: alice
└── maintainer: bob


55. Organizations
The registry SHOULD support organizations.
Example:
@lsharp/http
@lsharp/json
@forge/tools

The manifest could eventually support:
[package]
name = "@lsharp/http"
version = "1.0.0"

However, scoped package syntax should not be mandatory for ordinary packages.

56. Package Search
lpm search http

The registry should search:
package name
description
keywords
documentation
authors
repository metadata
Example:
$ lpm search http

http       2.1.0    HTTP client/server library
web        1.4.2    Web framework
https      0.9.0    HTTPS utilities


57. Package Information
lpm info http

Example:
Package:       http
Version:       2.1.0
License:       MIT
Downloads:     1,284,923
Repository:    github.com/lsharp/http
Documentation: registry.lsharp.dev/packages/http

Dependencies:
    tls ^1.2
    json ^2.0


58. Package Documentation
The registry should automatically generate documentation from LSharp source.
Example:
/// Creates a new HTTP server.
pub fn server(int port) -> Server {
    ...
}

The registry can display:
http.server()

Creates a new HTTP server.

Parameters:
    port: Server listening port.

Returns:
    Server

This makes package discovery substantially easier.

59. Package Quality Metadata
The registry may eventually show:
Version
Downloads
Repository
License
Documentation
Last updated
Dependencies
Supported LSharp versions
Supported platforms
CI status
Security advisories

The registry MUST NOT imply that popularity means security.

60. Security
The registry should support:
checksums
signed releases
package ownership
two-factor authentication
GitHub identity verification
security advisories
yanked versions
malware reporting
package transfer controls
LPM SHOULD verify package checksums automatically.

61. Package Cache
LPM maintains a local package cache.
Conceptually:
~/.lsharp/
├── packages/
├── registry/
├── cache/
└── credentials/

The exact filesystem location is platform-dependent.
Cached packages should be content-addressed where practical.

62. Offline Mode
LPM supports:
lpm install --offline

If all required dependencies are already cached, the project can be built without network access.

63. Reproducible Builds
The package system should strive for reproducible builds.
Given:
same source
same compiler
same lockfile
same dependencies
same target

the resulting build should be deterministic where the platform allows it.

64. Workspaces
LPM 1.0 SHOULD support workspaces.
Example:
project/
├── lsharp.toml
├── packages/
│   ├── server/
│   ├── client/
│   └── common/
└── ...

Root manifest:
[workspace]
members = [
    "packages/server",
    "packages/client",
    "packages/common"
]

This allows large LSharp projects to contain multiple packages.

65. Local Package Development
A package can be tested locally without publishing:
[dependencies]
mylib = {
    path = "../mylib"
}

This should be a core part of the development workflow.

66. Package Features
Eventually packages may expose optional features:
[features]
default = ["json"]
tls = ["openssl"]
database = ["sqlite"]

Users can enable them:
lpm add http --features tls

Feature support should be stabilized before the final 1.0 release if included.

67. Standard Library
The standard library ships with LSharp and does not require LPM.
Examples:
use math
use fs
use io
use net
use json
use time
use crypto
use random
use process
use env

Standard-library modules are versioned together with the LSharp language/runtime.
Third-party libraries are installed with:
lpm add package


68. Async
async fn download(str url) -> str {
    let response := await call http.get(url)
    return response.body
}

Spawn:
let task := spawn download(url)

Await:
let data := await task


69. Concurrency
Tasks:
let task := spawn {
    call process()
}

Channels:
let channel := call channel<int>(10)

call channel.send(42)

let value := await channel.receive()

The runtime MUST provide safe concurrency primitives.

70. Memory Management
LSharp should provide automatic resource management while allowing systems-level control.
Normal code should not require explicit free() calls.
Low-level functionality is available through:
unsafe {
    ...
}

The exact ownership and memory model MUST be formally specified before LSharp 1.0 finalization.

71. Unsafe Code
Unsafe operations require:
unsafe {
    ...
}

This provides a clear boundary between ordinary safe LSharp and low-level operations.

72. FFI
LSharp must support native interoperability.
At minimum, the reference compiler should support the C ABI.
Conceptual example:
extern fn printf(str format, ...)

Native library integration should be formally specified separately.

73. Attributes
Attributes use:
@test

Example:
@test
fn test_add() {
    let result := call add(2, 3)

    assert(result == 5)
}

Compiler attributes:
@inline
fn add(int a, int b) {
    return a + b
}

Deprecation:
@deprecated("Use new_function")
fn old_function() {
}


74. Testing
Tests can be located in:
tests/

or within source modules.
Example:
@test
fn test_add() {
    assert(call add(2, 3) == 5)
}

Run:
lpm test


75. Benchmarking
@benchmark
fn benchmark_sort() {
    call sort(data)
}

Run:
lpm bench


76. Formatting
Official formatter:
lformat .

Check:
lformat --check .

LSharp should have one canonical formatting style.

77. Linting
llint .

Linter checks may include:
unused variables
unused use declarations
unreachable code
suspicious conditions
deprecated APIs
unnecessary allocations
shadowing
inefficient patterns
package issues

78. Language Server
llsp provides:
autocomplete
diagnostics
go-to-definition
references
rename
hover information
signature help
formatting
code actions
documentation

79. Documentation
ldoc generates documentation:
ldoc build

Documentation comments:
/// Adds two integers.
pub fn add(int a, int b) -> int {
    return a + b
}

Package documentation should automatically be publishable to the LSharp Registry.

80. Compiler Architecture
The reference compiler should use:
Source
  ↓
Lexer
  ↓
Parser
  ↓
AST
  ↓
Name Resolution
  ↓
Type Checking
  ↓
HIR
  ↓
MIR
  ↓
Optimization
  ↓
Code Generation
  ↓
Object / Executable

This architecture should keep the compiler modular.

81. Target Platforms
LSharp 1.0 should target:
x86-64
ARM64

and:
Linux
Windows
macOS

The compiler architecture should permit future:
WASM
RISC-V
ARM32

targets.

82. Build Profiles
Debug:
lpm build --debug

Release:
lpm build --release

Release builds should enable appropriate optimization by default.

83. Complete Project
A mature LSharp application could look like:
myapp/
├── lsharp.toml
├── lsharp.lock
├── README.md
├── LICENSE
├── src/
│   ├── main.lsh
│   ├── server.lsh
│   ├── users.lsh
│   └── database.lsh
├── tests/
│   └── users_test.lsh
└── .github/
    └── workflows/
        └── publish.yml


84. Example Manifest
[package]
name = "myapp"
version = "1.0.0"
description = "A web application written in LSharp"
license = "MIT"
authors = ["Sasha"]
repository = "https://github.com/example/myapp"
homepage = "https://example.com"

[dependencies]
http = "^2.0.0"
json = "^2.1.0"
database = "^1.4.0"

[dev-dependencies]
test = "^1.0.0"


85. Example Application
use http
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


86. Example Package Workflow
Create:
lpm new my-http-app
cd my-http-app

Add dependencies:
lpm add http
lpm add json

Develop:
lpm run

Format:
lformat .

Lint:
llint .

Test:
lpm test

Build:
lpm build --release

Publish:
lpm login
lpm publish


87. GitHub Package Workflow
A library repository:
github.com/example/my-library

contains:
my-library/
├── lsharp.toml
├── src/
├── tests/
└── .github/
    └── workflows/
        └── publish.yml

Developer creates:
v1.0.0

GitHub Release.
GitHub Actions:
checkout
   ↓
install LSharp
   ↓
lpm test
   ↓
lpm build
   ↓
authenticate with registry
   ↓
lpm publish

Registry:
my-library@1.0.0

becomes available through:
lpm add my-library


88. Registry Architecture
The eventual official registry should consist of several services.
                LSharp Registry
                       │
        ┌──────────────┼──────────────┐
        │              │              │
     Registry        Storage        Search
       API            │              │
        │             │              │
        ├──────────────┤              │
        │                             │
   Authentication                 Metadata
        │                             │
        └──────────────┬──────────────┘
                       │
                     LPM

The registry API handles:
authentication
package publishing
package downloads
version lookup
dependency metadata
ownership
yanking
security advisories

Object storage handles package archives.
Search handles:
name
description
keywords
documentation
authors


89. Registry API
LPM should communicate with the registry through a documented API.
The API should support:
GET     package metadata
GET     package version
GET     package archive
POST    publish package
POST    yank version
POST    unyank version
GET     search
GET     owner information
POST    ownership operations

The exact API protocol is implementation-defined but should be versioned.

90. Alternate Registries
LPM should support custom registries.
Example:
lpm registry add company https://packages.company.com

Then:
[registries]
company = "https://packages.company.com"

This allows companies and organizations to host private package registries.

91. Private Packages
Private packages can exist in private registries:
[dependencies]
company-auth = {
    registry = "company",
    version = "^2.0"
}

This makes LSharp suitable for enterprise development.

92. Package Authentication
LPM supports:
lpm login

Authentication can use:
browser login
API tokens
CI credentials
GitHub OIDC
organization credentials
Credentials MUST be stored securely using the platform's credential facilities where possible.

93. GitHub Account Linking
Users may link their LSharp Registry account to GitHub.
This allows the registry to verify:
GitHub account
repository ownership
organization membership
release identity

This is especially useful for automated publishing.

94. GitHub Repository Verification
A package can optionally declare:
[package]
repository = "https://github.com/example/http"

The registry can verify that the publisher has permission over that repository.
This helps prevent malicious users from claiming legitimate projects.

95. Package Provenance
Published packages SHOULD contain provenance information:
package
version
source repository
commit
Git tag
publisher
build environment
compiler version

Example:
http@2.1.0

Source:
github.com/lsharp/http

Commit:
a83f91c

Tag:
v2.1.0

Published by:
github:sasha

This provides a chain between source code and package release.

96. Package Security Advisories
The registry should support advisories:
http < 2.1.4
SEVERE
HTTP request smuggling vulnerability

LPM can warn:
warning: dependency `http@2.1.2` has a security advisory

  vulnerability: LS-2028-001
  severity: high

  recommended version: >=2.1.4

Eventually:
lpm audit

should scan the dependency tree.

97. lpm audit
Example:
lpm audit

Output:
Auditing 24 dependencies...

1 vulnerability found

http 2.1.2
HIGH
Upgrade to 2.1.4 or later.


98. Dependency Tree
LPM should provide:
lpm tree

Example:
myapp
├── http 2.1.4
│   ├── tls 1.4.0
│   └── json 2.1.0
├── database 1.3.2
│   └── sqlite 4.0.1
└── logging 1.0.0

This is useful for debugging dependency conflicts.

99. Package Cache and Offline Builds
LPM should cache downloaded packages.
If all dependencies are cached:
lpm build --offline

should work without registry access.

100. Package Registry Compatibility
The official registry should be designed so that:
LPM
    ↓
Official Registry

is the default, but:
LPM
    ↓
Private Registry

and:
LPM
    ↓
Git Repository

are also legitimate workflows.
The package manager should never make the language dependent on one particular registry implementation.

101. LSharp Versioning
Language versions use:
MAJOR.MINOR

Example:
1.0
1.1
2.0

Breaking language changes require a new major language version.
Projects may specify:
[language]
version = "1.0"

The compiler must not silently reinterpret old source code.

102. Compatibility
Within LSharp 1.x:
existing valid source should remain valid
semantics should not silently change
package APIs should follow semantic versioning
compiler diagnostics may improve
optimization may change generated machine code
Breaking language changes require LSharp 2.0.

103. Error Diagnostics
Compiler errors should be highly readable.
Example:
error[E1024]: type mismatch

 --> src/main.lsh:12:5
  |
12 | age := "twenty"
  |        ^^^^^^^^ expected `int`
  |
  = note: `age` was declared as `int`

Diagnostics should include:
error code
file
line
column
source snippet
explanation
suggestions where possible

104. Official Repository Structure
The official LSharp project can eventually be organized as:
lsharp/
├── compiler/
│   ├── lexer/
│   ├── parser/
│   ├── ast/
│   ├── resolver/
│   ├── typechecker/
│   ├── hir/
│   ├── mir/
│   ├── optimizer/
│   └── backend/
│
├── lpm/
│   ├── resolver/
│   ├── registry/
│   ├── cache/
│   ├── lockfile/
│   └── cli/
│
├── lformat/
├── llint/
├── llsp/
├── ldoc/
│
├── std/
│   ├── core/
│   ├── collections/
│   ├── fs/
│   ├── io/
│   ├── net/
│   ├── http/
│   ├── json/
│   ├── math/
│   ├── time/
│   ├── crypto/
│   └── sync/
│
├── registry/
│   ├── api/
│   ├── storage/
│   ├── search/
│   └── auth/
│
├── tests/
├── examples/
├── docs/
└── SPEC.md


105. LSharp 1.0 Definition of Done
LSharp should not be called 1.0 merely because the compiler can execute simple programs.
LSharp 1.0 requires:
Language
lexer
parser
type checker
functions
variables
structs
enums
generics
interfaces
modules
use
control flow
error handling
async/concurrency
standard library
defined memory model
defined FFI model
defined unsafe model
Compiler
debug builds
release builds
Linux
Windows
macOS
x86-64
ARM64
good diagnostics
optimization
reproducible builds where supported
LPM
project creation
dependency installation
dependency resolution
lockfiles
version ranges
local dependencies
Git dependencies
registry dependencies
package caching
offline mode
publishing
authentication
package search
package information
dependency tree
security auditing
Registry
package API
package storage
versioning
checksums
package search
documentation
ownership
authentication
yanking
security advisories
GitHub integration
automated publishing
provenance
private registries
Tooling
formatter
linter
language server
documentation generator
test runner
benchmark runner
editor integrations

106. The LSharp Ecosystem
The intended final ecosystem is:
                        LSharp
                           │
             ┌─────────────┼─────────────┐
             │             │             │
            lsc            lpm        Tooling
         Compiler       Packages       │
             │             │       ┌─────┼─────┐
             │             │    lformat llint llsp
             │             │
             │       LSharp Registry
             │             │
             │      ┌──────┼──────┐
             │      │      │      │
             │    Search  Docs  Packages
             │             │
             │          GitHub
             │             │
             │       ┌─────┴─────┐
             │       │           │
             │    Source      Releases
             │                   │
             │              GitHub Actions
             │                   │
             └───────────────→ LPM

The normal developer experience should ultimately be:
lpm new myapp
cd myapp

lpm add http
lpm add json

lpm run
lpm test
lformat .
llint .

lpm build --release

And for a library:
lpm new --lib mylibrary
cd mylibrary

lpm test
lpm build --release
lpm publish

Or through GitHub:
git push
    ↓
GitHub
    ↓
Release v1.0.0
    ↓
GitHub Actions
    ↓
LSharp authentication
    ↓
lpm publish
    ↓
LSharp Registry
    ↓
lpm add mylibrary

That gives LSharp the full ecosystem loop:
write → build → test → package → publish → discover → install → update → audit.

107. Core LSharp Example
The language itself should remain significantly smaller than the ecosystem around it:
use http

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

The goal is that someone can read this without knowing the compiler internals, package manager, registry, or runtime.

108. Final LSharp 1.0 Toolchain
The official LSharp installation ultimately provides:
lsc       Compiler
lpm       Package manager + project manager
lformat   Formatter
llint     Linter
llsp      Language server
ldoc      Documentation generator

And the ecosystem provides:
LSharp Registry
GitHub integration
Package documentation
Package search
Security advisories
Private registries
Automated publishing
Dependency resolution
Reproducible builds

LPM is therefore not merely "the LSharp equivalent of pip."
It is the official project/package/build interface for the entire LSharp ecosystem.
pip manages Python packages.
LPM manages LSharp projects, dependencies, builds, tests, packages, and publishing.

