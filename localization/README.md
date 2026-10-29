# Editable localization table

The application localization data is one file: [translations.csv](translations.csv).
It is UTF-8 with a BOM, comma separated, and RFC4180 quoted, including embedded
quotes and newlines. Import it into Excel as UTF-8/comma-delimited text; keep
the key and language-code headers as text. Save back as UTF-8 CSV.

The first column is `key`. The next columns start with `en-US`, `es-ES`,
`ru-RU`, `uk-UA`, `de-DE`, followed by every other locale supplied with the
OBS frontend. `SkyLive.LanguageName` contains the native name of each language.
Do not translate or rename keys or locale headers. Preserve `%1`, `%2`, `%n`,
`{name}`, numeric placeholders, newlines and escape-sensitive text in messages.

Key scopes:

- Frontend OBS keys retain their original identity, for example `Basic.Main.StartStreaming`.
- Built-in module strings use `Module.<module-id>.<original-key>`.
- Application additions use stable `SkyLive.*` keys. Automatically discovered
  unmatched English literal candidates use `SkyLive.Text.<slug>.<hash>`.
- Catalog descriptions use `Plugin.<provider-id>.Description` (or a versioned description key when its meaning changes); the original
  Russian description is retained and the other required languages are seeded.

Empty translation cells are intentional and fall back to English. They are
not filled with English to claim translation coverage. Nonempty-cell counts
describe data coverage; they do not establish translation quality, font
coverage, RTL layout or native UI acceptance. Automatically discovered literals
include diagnostic templates and require contextual review. Runtime device
names, filenames, authored scene names and source identities are not translated.

Spanish, Russian, Ukrainian and German translations for all current user-facing
application-specific labels, diagnostics and templates were authored by the
assistant in the generator, `app-seeds.json` and `tooltip-seeds.json`, without
native-linguist review.
Plugin-description seeds likewise preserve original Russian and use
assistant-authored EN/ES/UK/DE text. The other locale strings come from the
supplied OBS sources. `app-invariants.json` separately identifies technical
units, brands, widget IDs and headless-test text; these stay unchanged and are
not presented as translated user-facing rows. Other OBS languages retain empty
application cells and use English fallback.

The current Source13 source table retains all 77 supplied locale columns and
adds camera controls, NDI, monitoring, slideshow, Craft effects, toolbars,
virtual-camera and runtime-statistics captions/help in EN/ES/RU/UK/DE.
Exact camera-control keys are contextual: camera Pan does not replace audio Pan.
Theme brands, executable identities and opaque runtime-value templates retain
their literal spelling and are not counted as translated interface sentences.
The Source11 candidate preserves its own earlier table separately. Technical
IDs/units/compositions remain explicit invariants, rather than false translations.
Authored data does not establish native-linguist or full runtime acceptance.
The remaining72 locale columns retain untranslated application gaps and English
fallback. Earlier selected public UI strings were processed by the Google
endpoint with explicit `machine_unreviewed` provenance. HTTP429 stopped that run;
no workaround, automatic retry or background translator remains. Exact
unsupported language variants are not filled with neighboring languages.

The 2026-10-27 offline reuse pass filled 5,517 previously empty cells using
unambiguous matching OBS text, earlier recorded local translations and explicit
technical invariants; the count is not a count of newly translated sentences.
Existing nonempty cells were retained. Other gaps remain and continue to fall
back to English. The associated status and provenance files describe this pass;
translation quality and display have not been checked in the application.

The optional locally generated `translation-provenance.json` cache distinguishes original-source alias reuse,
English-variant reuse, assistant-reviewed source supplements, genuine invariants
and unreviewed service output, with source/value hashes. `translation-status.json`
records the exact remaining gaps by namespace/language and the service gate.
`placeholder-qc.json` documents the generated-cell QC scope. Seven own malformed
machine filename-mask results were repaired from an explicitly approved
language-independent source mask; one own unchanged English result was rejected
to an empty gap. Before/after records are retained separately. No unrelated or
user-edited cell was replaced by these repairs.

To check coverage without writing either the CSV or report:

```powershell
python tools/localization_catalog.py --check
```

To merge newly discovered source keys/languages:

```powershell
python tools/localization_catalog.py
```

Every existing CSV cell, **including an empty cell**, is preserved by the merge.
New keys and columns are seeded from sources; existing edited rows are not reset
to upstream values. This conservative policy also preserves stale values until
an editor intentionally updates them. `--check` success means structural/source
key coverage, not that every language has a translation for every row.

The separately requested completion of our generated application rows can be
reproduced explicitly with:

```powershell
python tools/localization_catalog.py --complete-app
```

This opt-in operation fills only still-empty `SkyLive.*` cells from the reviewed
ES/RU/UK/DE dictionary. It preserves all nonempty cells, all source keys and all
frontend/module translations. Unlike the ordinary merge, it also fills an
intentionally empty application cell if a dictionary translation exists. The
JSON dictionaries are generator inputs/review metadata; the runtime continues
to consume only the single `translations.csv` table.

`python tools/translate_localization.py --reuse-only` performs only offline
reviewed-source/invariant/cache replay. `--run` is a separately bounded network
operation, defaults to one worker and globally spaces requests at least two
seconds apart. It only admits unchanged generated public English, skips edited
English and authored values, preserves nonempty cells, protects format tokens,
and stops on HTTP403/429. It must not be used to bypass a service rejection.

The locally generated `catalog-report.json` records data counts, missing translations, English-source
locations, module locale directories and source parsing warnings. It is a tool
report, not a second runtime language table. The generator follows the supplied
`libobs/util/text-lookup.c` escape rules. Its handling of four malformed/trailing
translation lines is reported explicitly instead of silently repairing OBS data.

Run isolated CSV/merge tests with:

```powershell
python tools/localization_catalog.py --self-test
```

The runtime reload action applies an edited table after validation; changing the
file alone is not a live watcher. Supplied OBS translation text retains its upstream licensing obligations.
See [LICENSE](../LICENSE), [OBS attribution](../third-party/obs-studio/README.md),
and the component notices under [assets/licenses](../assets/licenses/). Some
catalog-generation tools require local reference material that is not included
in the public repository.

Packaging uses `python tools/package_resources.py --destination <app-directory>`.
The helper preserves edited destination translations; an unedited file from a
previous package can be upgraded using its receipt hash. Explicit
`--replace-localization` permits replacement of edited translations. Both the
CSV and receipt use create-new temporary files in their destination directories,
flush/fsync and atomic replacement. The CSV is published first. If subsequent
receipt publication fails, the tool reports `after_resource_publication`; the
new resource remains valid and a subsequent invocation can repair its receipt.
This is not a single atomic transaction across both files. Resolved resource
paths must remain within the selected destination.

Run isolated temporary-directory packaging tests with
`python tools/package_resources.py --self-test`.
