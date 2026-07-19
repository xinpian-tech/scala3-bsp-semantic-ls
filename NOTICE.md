# Third-party notices

## Ported presentation-compiler test corpus (Scala 3 / dotty)

The munit suites under `modules/ls-pc/test/src/ls/pc/corpus/` contain test
cases and test-harness code ported from the Scala 3 ("dotty")
presentation-compiler test suite, version 3.8.4:

- https://github.com/scala/scala3/tree/3.8.4/presentation-compiler
  (`test/dotty/tools/pc/base/`, `test/dotty/tools/pc/utils/`,
  `test/dotty/tools/pc/tests/`)

Scala 3 is licensed under the Apache License, Version 2.0
(http://www.apache.org/licenses/LICENSE-2.0):

> Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
>
> Licensed under the Apache License, Version 2.0 (the "License"); you may not
> use this file except in compliance with the License. Unless required by
> applicable law or agreed to in writing, software distributed under the
> License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS
> OF ANY KIND, either express or implied. See the License for the specific
> language governing permissions and limitations under the License.

Per Apache License 2.0 section 4, the ported files carry a source-attribution
header and a statement of changes (re-homed from JUnit 4 onto munit and onto
the `ls.pc.PcFacade` surface; curated subsets, no compat maps).

## metals (Scalameta)

The corpus selection and harness design were cross-referenced against the
presentation-compiler cross tests of scalameta/metals
(https://github.com/scalameta/metals, `tests/cross/`, `tests/mtest/`), also
licensed under the Apache License, Version 2.0:

> Copyright 2018-2026 Scalameta contributors

No metals source text is currently vendored in this repository; should cases
be ported from metals directly, they must carry the equivalent attribution
header pinned to the metals commit of origin.
