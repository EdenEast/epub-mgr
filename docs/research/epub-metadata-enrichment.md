# EPUB metadata enrichment from title and author

Date: 2026-07-31

Question: when an EPUB only has a book title and author, which primary source/API is best for enriching metadata automatically?

Sources used: only first-party API docs, specs, terms, and first-party API examples from the compared services.

## Recommendation

Use **Open Library Search API as the default discovery/enrichment source**, with a local confidence scorer and optional cross-checks from **Google Books** and **Wikidata**. Do not make ISBNdb the default unless the application accepts a paid dependency; do not use Crossref except for scholarly books/chapters; do not use Library of Congress JSON/YAML as a general book catalog lookup.

Open Library is the best fit for title+author automation because it exposes title/author search, work/edition identifiers, ISBN/OCLC/LCCN, subjects, language, publication dates, publishers, cover IDs, ratings/popularity signals, bulk dumps, and indexed series fields (`series_key`, `series_name`, `series_position` in Open Library's work search schema), while its data is designed to be reusable and mirrorable. If the application only cares about title, author, series, and series number, Open Library remains the best first lookup; Wikidata is the best secondary confirmation source because it models `part of the series` (P179) with `series ordinal` (P1545), but coverage is too sparse to use alone. Google Books is less useful for this narrowed case because its documented Volume resource includes title/authors but no first-class series or series-number fields; keep it as a broad title/author cross-check rather than the source of series truth.

## Summary matrix

| Source | Lookup from title + author | Coverage fit | Useful fields | Access/rate/terms | Licensing/reuse | Automation suitability |
|---|---:|---:|---|---|---|---:|
| Open Library | Strong | Broad book catalog, works + editions | work/edition IDs, ISBN, OCLC, LCCN, authors, subjects, language, publish dates, publishers, covers | Public APIs plus bulk dumps; cover API should not be crawled | Open Library publishes bulk data and API docs for reuse | Best default |
| Google Books | Strong | Broad commercial/consumer visibility; variable completeness | title/subtitle, authors, publisher/date, description, categories, ISBNs, language, thumbnails, preview links | API key/OAuth; Google API and Books terms apply | Terms restrict use, charging, and require removals | Good secondary; avoid as canonical store |
| Library of Congress | Weak for general title+author API | Excellent MARC corpus, but JSON/YAML API is for loc.gov digital objects rather than full catalog | MARC records, LCCN, subjects, names, publication data | Legal page recommends low request rates; MARC distribution available | LOC records/data access are public-sector-friendly, but confirm each dataset | Good offline authority source, not default live lookup |
| ISBNdb | Good when title+author endpoint returns match | Commercial book metadata, ISBN-oriented | ISBNs, titles, authors, publisher, dates, synopses, images | API key; paid plans with daily and per-second limits; 401/429 documented | Commercial terms/subscription | Good paid fallback for ISBN discovery |
| Crossref | Moderate | Scholarly books/chapters/member deposits | DOI, ISBN, publisher, authors, dates, language, references, URLs | REST API; polite usage expected | Metadata intended for retrieval/reuse through Crossref APIs | Use only for scholarly/DOI works |
| Wikidata | Moderate | Strong for notable works/people, sparse for many editions | QIDs, identifiers, sitelinks, authors, editions, publication data, languages, subjects | API, dumps, SPARQL; query-service ToU and user-agent norms | CC0 | Best for reconciliation/IDs, not primary discovery |
| EPUB 3.3 constraints | N/A | Format constraint | required/optional package metadata vocabulary | W3C spec | W3C spec license | Defines what enriched fields may be written |

## Source-by-source findings

### Open Library

Primary sources:

- Search API: <https://openlibrary.org/dev/docs/api/search>
- Covers API: <https://openlibrary.org/dev/docs/api/covers>
- Bulk data/dumps: <https://openlibrary.org/developers/dumps>
- Developers/API index: <https://openlibrary.org/developers/api>

Open Library Search API supports `/search.json` and fielded parameters, including title and author-oriented search. Results expose work keys and many edition-derived fields such as ISBNs, OCLC numbers, LCCNs, author keys/names, first publish year, publishers, language, subjects, and cover IDs. Covers can be fetched by identifiers such as OLID, ISBN, OCLC, LCCN, and internal cover ID, but Open Library's Covers API documentation warns against crawling covers indiscriminately.

Best fit:

- Initial lookup by normalized title + author.
- Candidate set generation: fetch top N works/editions, then score locally.
- Canonical internal identifiers: keep Open Library work key and edition keys.
- Bulk/offline enrichment: use dumps rather than crawling.

Main risks:

- Community/catalog data can contain duplicates, merged works, inconsistent editions, and variant author/title strings.
- A work match is not necessarily the exact edition in the EPUB. Treat edition-level metadata as uncertain unless ISBN/LCCN/OCLC confirms it.

### Google Books

Primary sources:

- Books API v1 usage docs: <https://developers.google.com/books/docs/v1/using>
- Google Books API terms: <https://developers.google.com/books/terms>

Google Books API supports volume search with query terms and operators, including title/author-oriented queries. `volumeInfo` commonly includes title, subtitle, authors, publisher, publishedDate, description, industry identifiers, page count, categories, average rating, language, image links, preview links, and canonical links.

Best fit:

- Secondary enrichment and tie-breaker after Open Library.
- Human-facing fields such as descriptions, thumbnails, categories, preview links.

Main risks:

- Terms are restrictive for long-term metadata harvesting or republishing. Google Books terms include constraints around charging users, attribution/presentation, compliance with removal requests, and allowed API use.
- Results are volume records from Google's index, not a neutral bibliographic authority.

### Library of Congress

Primary sources:

- JSON/YAML API: <https://www.loc.gov/apis/json-and-yaml/>
- MARC Distribution Services / MARC Open Access: <https://www.loc.gov/cds/products/marcDist.php>
- LOC legal / requests guidance: <https://www.loc.gov/legal/>

The LOC JSON/YAML API is for `loc.gov` items and digital collections, not a complete live API over the full Library of Congress catalog. It can return rich bibliographic-like data for items represented on `loc.gov`, but it is not the best general title+author lookup surface for consumer EPUB metadata enrichment.

LOC MARC distribution is the stronger bibliographic source: the MARC Open Access dataset includes a large retrospective corpus of MARC records. It is better suited to offline indexing, authority reconciliation, LCCN validation, subject headings, and high-quality library metadata.

Best fit:

- Offline/imported authority source, especially if the app needs LCCN, MARC subjects, library names, or high-trust bibliographic records.
- Cross-checking candidates found elsewhere.

Main risks:

- Live JSON/YAML API coverage is not general catalog coverage.
- LOC legal guidance recommends modest request rates and may block abusive automated traffic.

### ISBNdb

Primary sources:

- API docs: <https://isbndb.com/apidocs/v2>
- Pricing/rate limits: <https://isbndb.com/pricing>

ISBNdb is explicitly ISBN/book metadata oriented and supports authenticated API access using an `Authorization` header. The docs describe status codes including 401, 400, 404, and 429, and rate-limit headers. Public pricing lists daily and per-second limits: Basic 5,000/day at 1 request/sec, Premium 15,000/day at 3 requests/sec, Pro 50,000/day at 5 requests/sec, and Enterprise 200,000/day at 10 requests/sec.

Best fit:

- Paid lookup when ISBN discovery is valuable and predictable API quotas are acceptable.
- Fallback when Open Library/Google candidates are ambiguous.

Main risks:

- Paid dependency and quota ceilings.
- ISBN-oriented data can still confuse editions; title+author-only lookup should be scored and verified before writing identifiers.

### Crossref

Primary sources:

- REST API documentation: <https://www.crossref.org/documentation/retrieve-metadata/rest-api/>
- REST API tips: <https://www.crossref.org/documentation/retrieve-metadata/rest-api/tips-for-using-the-crossref-rest-api/>
- API endpoint/swagger: <https://api.crossref.org/swagger-ui/index.html>

Crossref REST API supports bibliographic search fields such as `query.title` and `query.author`, and filters such as `type:book`. A title+author query can return book metadata including DOI, ISBN, publisher, authors, dates, language, and URLs.

Best fit:

- Scholarly books, book chapters, proceedings, academic publishers, DOI-bearing works.
- DOI enrichment after a likely book match.

Main risks:

- Crossref metadata is member-deposited scholarly/publisher metadata, not comprehensive consumer ebook metadata.
- A strong Crossref score for a chapter/proceedings item may be a false positive for a whole-book EPUB.

### Wikidata

Primary sources:

- Data access: <https://www.wikidata.org/wiki/Wikidata:Data_access>
- Database downloads: <https://www.wikidata.org/wiki/Wikidata:Database_download>
- Licensing: <https://www.wikidata.org/wiki/Wikidata:Licensing>
- Query service / SPARQL: <https://query.wikidata.org/>
- Wikimedia user-agent policy: <https://foundation.wikimedia.org/wiki/Policy:Wikimedia_Foundation_User-Agent_Policy>

Wikidata is excellent for structured identifiers and reconciliation. Its data is CC0, available through APIs, dumps, and SPARQL, and book/work entities often link to external identifiers. It can connect works, editions, authors, languages, identifiers, sitelinks, subjects, and publication data.

Best fit:

- Entity reconciliation: identify author QID and work QID after an Open Library/Google candidate.
- Identifier graph: ISBN, VIAF, Open Library IDs, Library of Congress IDs, Project Gutenberg IDs, DOI where present.
- Offline bulk matching via dumps for reuse-safe data.

Main risks:

- Coverage is skewed toward notable works and authors; many ordinary editions are absent.
- SPARQL service is not suitable for high-volume per-book live enrichment unless the app obeys service limits and uses caching/bulk dumps.

### EPUB 3.3 metadata constraints

Primary source:

- EPUB 3.3 W3C Recommendation, Package document metadata: <https://www.w3.org/TR/epub-33/#sec-pkg-metadata>

EPUB 3.3 constrains what the application should write back into an EPUB package document:

- The package `metadata` element must contain at least one `dc:identifier`, `dc:title`, and `dc:language`, plus exactly one `meta property="dcterms:modified"` value.
- `dc:identifier` is repeatable; the `package` element's `unique-identifier` attribute points at the EPUB publication's primary identifier. EPUB creators should not change the unique identifier for minor metadata edits.
- `dc:title` is repeatable, but the first title is the main display title; EPUB creators should usually use a single title for consistent reading-system behavior.
- `dc:language` values must be well-formed BCP 47 language tags; the first is treated as primary.
- Optional Dublin Core elements include `dc:creator`, `dc:contributor`, `dc:date`, `dc:description`, `dc:publisher`, `dc:subject`, `dc:rights`, `dc:source`, and others.
- Refinements can express `file-as`, `role`, `alternate-script`, `identifier-type`, `title-type`, `authority`, `term`, `belongs-to-collection`, and `group-position`.
- Cover images are represented in the manifest with the `cover-image` property, not by arbitrary external thumbnail URLs.
- Linked metadata records can be associated with `<link rel="record" ...>`, but reading systems are not required to process linked records.

Practical implication: enrichment should update only fields that map cleanly into EPUB metadata and preserve existing publication identity. Prefer adding source identifiers (`urn:isbn:...`, DOI, LCCN, Open Library IDs as non-unique auxiliary identifiers or linked records) over replacing the package unique identifier.

## Matching and dedupe approach

Recommended automated pipeline:

1. **Normalize input**: trim whitespace, Unicode-normalize, case-fold for comparison, strip leading articles for scoring only, normalize punctuation, split title/subtitle, and normalize author display vs sort forms.
2. **Open Library search**: query title + author, request useful fields, retain top candidates.
3. **Candidate scoring**:
   - exact/near title match, including subtitle handling;
   - primary author name match and known aliases;
   - edition language vs EPUB language;
   - publication year proximity if EPUB has an existing date;
   - identifier overlap if EPUB already has ISBN/LCCN/OCLC/DOI;
   - cover/title consistency only as weak evidence;
   - reject candidates whose type/format clearly contradicts the EPUB.
4. **Edition selection**: separate work-level data from edition-level data. Use work for subjects/series/general identity; use edition only when ISBN/publisher/date/language support it.
5. **Cross-check ambiguous matches**: call Google Books and/or ISBNdb for ISBN/date/publisher and Wikidata for entity IDs. Use Crossref only when input or candidate indicates academic/DOI content.
6. **Dedupe identifiers**: normalize ISBN-10/13, DOI case/prefix, LCCN/OCLC formats, and Open Library work/edition keys. Store provenance per field.
7. **Write conservatively**: never overwrite non-empty EPUB metadata without a higher-confidence source or user confirmation. Prefer proposed changes for human review when confidence is below threshold.

Suggested confidence thresholds:

- **Auto-apply safe**: strong title match, strong author match, and at least one identifier match or two independent source agreements.
- **Prompt user**: strong title/author but edition conflicts, multiple same-title candidates, or no identifier support.
- **Do not apply**: title/author fuzzy only, anthology/series ambiguity, translated title mismatch, or Crossref chapter/article result for a book EPUB.

## Field mapping into EPUB

| Enriched field | EPUB mapping | Notes |
|---|---|---|
| Title | `dc:title` | Keep one main title unless user opts into refinements. |
| Subtitle/alternate title | single combined `dc:title` or refined title metadata | EPUB support for multiple title elements is inconsistent. |
| Author | `dc:creator` | Add `meta property="role" scheme="marc:relators">aut</meta` where useful. |
| Sort author | `meta refines="#creator" property="file-as"` | Useful for library display. |
| Language | `dc:language` | Must be BCP 47. Do not guess low-confidence languages. |
| Publication date | `dc:date` | EPUB allows at most one `dc:date`; choose edition publication date only if confident. |
| Publisher | `dc:publisher` | Edition-level field; avoid work-level guessing. |
| Description | `dc:description` | Beware Google terms if storing/reusing descriptions. |
| Subjects/categories | `dc:subject`; optionally `authority` + `term` refinements | Use scheme when source provides controlled subject authority. |
| ISBN/DOI/LCCN/OCLC | additional `dc:identifier` with `identifier-type` refinement | Do not replace unique identifier by default. |
| Source print ISBN | `dc:source` plus `identifier-type`; optionally `source-of` | Useful when EPUB follows print pagination. |
| Series/collection | `meta property="belongs-to-collection"`; `collection-type`; `group-position` | Only when source evidence is strong. |
| Cover | manifest item with `properties="cover-image"` | Downloaded external covers must be license/terms-safe. |
| External record | `link rel="record" href="..." media-type="..."` | Reading systems may ignore linked records. |

## Final ranking

1. **Open Library**: best default source for title+author discovery and reusable automation.
2. **Google Books**: best secondary source for broad consumer coverage and descriptions/thumbnails, but constrained by terms.
3. **Wikidata**: best reusable reconciliation/identifier graph, especially through dumps.
4. **ISBNdb**: good paid fallback for ISBN-oriented enrichment.
5. **Library of Congress**: high-quality authority/MARC source, best used offline or for cross-checking rather than live general lookup.
6. **Crossref**: excellent for DOI/scholarly books, poor as a general EPUB metadata source.

## Implementation recommendation for epub-mgr

Implement a provider interface with Open Library as the first provider and explicit provenance on every returned field. Add optional providers in this order: Google Books, Wikidata, ISBNdb, LOC MARC/offline index, Crossref. The merge layer should be independent of providers and should output a proposed EPUB metadata patch plus confidence/explanations, not mutate files directly.

Default policy:

- Automatically enrich only high-confidence matches.
- Cache all API responses and obey each provider's user-agent/rate guidance.
- Prefer reusable fields from Open Library/Wikidata/LOC for stored metadata.
- Treat Google descriptions/thumbnails and ISBNdb data as provider-attributed, terms-bound data.
- Keep the EPUB package's existing unique identifier unless the user explicitly asks to replace it.
