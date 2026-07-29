# Kyno (Lesspain Software → Signiant) — Research Report

## 1. Core purpose, target user, current status

**Purpose:** Cross-platform (macOS + Windows) desktop "media browser and workflow swiss-army-knife" — described by its own marketing as "the centerpiece of professional video workflows for narrative, documentary, corporate and social media projects" ([lesspain.software/kyno/features/](https://lesspain.software/kyno/features/)). Positioned deliberately as a "thin MAM": client-side, no server, no database, no ingest. Richard Lackey's review frames the category distinction precisely — a "fat" MAM runs services on a dedicated server with a centralized metadata database; Kyno "runs client side instead of on a server" ([richardlackey.com](https://www.richardlackey.com/kyno-review-media-management-for-video-creators/)).

**Target user:** Explicitly spans the skill range — "simple enough to use for directors and production assistants, powerful & flexible enough for seasoned DOPs, DITs and editors" ([lesspain.software](https://lesspain.software/)). Listed audiences: camera/drone owners, editors, VFX artists, cinematographers, journalists, photographers, producers, YouTubers, social marketing. Three named workflow contexts: on-location (backup, preview, annotate, shotlists, dailies), post-production (screen, select, transcode proxies, send to NLE), and shared storage (tag/find on NAS/SAN in a team).

**Status — this is the most misunderstood part of the story.** Not discontinued, but it went through a ~4-year dormancy that the community widely read as death.

- Signiant acquired Lesspain Software March 12, 2021; announced March 16, 2021. Stated rationale: "Lesspain's talent and technology will be used to extend the functionality of Signiant's SDCX SaaS platform" — i.e. the acquisition was primarily an acqui-hire for the media-processing engine, with Kyno-the-desktop-app as a secondary asset ([signiant.com newsroom](https://www.signiant.com/newsroom/signiant-acquires-lesspain-software-to-enhance-sdcx-saas-platform/)). Core Lesspain team stayed in Germany. Signiant's FAQ promised "the Kyno product remains fully supported" with no price changes ([acquisition FAQ](https://lesspain.software/kyno/pages/news/signiant-inc.-acquires-lesspain-software/)).
- Reality diverged. Through 2021–2022 users publicly asked "@Signiant did you guys kill Kyno?" and reported being unable to *buy* a copy post-acquisition; Scott Simmons noted a Signiant CEO interview that was "very non-committal… as far as Kyno's future is concerned" ([ProVideo Coalition, Feb 2022](https://www.provideocoalition.com/an-update-on-kyno-and-what-might-be-in-store-for-the-future-of-this-fantastic-piece-of-post-production-software/)). Signiant also acquired Reach Engine in Nov 2021, compounding the "where does Kyno fit" question.
- By Nov 2023 the NeoFinder team wrote that Kyno "seems to be dead in the water, leaving all its users in the dark" and shipped a metadata-rescue/migration feature ([NeoFinder forum](https://www.neofinder.de/forum/phpBB3/viewtopic.php?t=200)).
- **September 2025: Kyno 1.9 shipped** — the first substantial release in years. New darker UI, **native Apple Silicon build** (previously Rosetta-only), updated BRAW and RED RAW SDKs, background non-blocking pre-analyze, smart proxy presets, shared-filesystem storage for technical metadata and thumbnails, Premiere Pro 2025 compatibility ([lesspain.software 1.9 release](https://lesspain.software/kyno/pages/news/kyno-1.9-release/); [Newsshooter](https://www.newsshooter.com/2025/09/11/kyno-version-1-9/) — "A lot of us probably thought Kyno was dead and buried"). Note: **auto-update keeps Apple Silicon users on the Intel build**; the ARM version must be downloaded manually.
- **Features were removed in 1.9: Frame.io integration and Archiware P5 integration are gone** ([Digital Production, Sept 2025](https://digitalproduction.com/2025/09/19/kyno-update-still-alive-still-useful/)). Digital Production's verdict: "no revolution, but it proves the tool is alive and slowly evolving… consider this release a maintenance update."

## 2. Key features

**Browse without import.** "Browse your videos directly from your hard drive or SD card, no ingest required." The signature feature is **Drilldown** — flattens an entire volume or folder tree into a single thumbnail view, with media-aware filters (framerate, resolution, format, codec). A user with 120 shows across 120 folders described it as letting them see all b-roll/rushes/stock/graphics as one page of thumbnails: "we don't have to constantly click into folders… a huge time saver" ([Pixel Valley Studio](https://pixelvalleystudio.com/pmf-articles/kyno-media-management-for-video-editing)).

**Playback.** "Play virtually any format, including RED RAW R3D and Blackmagic RAW, with an accurate state-of-the-art video player." HEVC, MXF, P2, XDCAM, DNxHD/HR. LUT preview for log footage (camera and creative LUTs). Also handles stills (incl. many RAW formats) and audio. A **Content tab** shows a thumbnail filmstrip of an entire clip so you can assess it without scrubbing. Notably, users on decade-old Macs reported Kyno playing 4K smoothly where the NLE stuttered ([InspirePilots forum](https://inspirepilots.com/threads/found-a-useful-media-browser-for-mac.15625/)).

**Screening/review/logging.** Markers with titles/descriptions for logging; batch still export from markers; Excel shotlist export for team communication. Frame.io integration for dailies review was added in 1.6 ([Larry Jordan](https://larryjordan.com/articles/kyno-adds-significant-features-in-1-6-1-update/)) — **removed in 1.9**.

**Subclipping.** Set in/out points on long takes (drone, action cam) and export subclips. Most subclip exports happen **without re-transcoding** (rewrap), making them near-instant.

**Tagging/rating/metadata.** Custom tags, star ratings, descriptions, camera/reel/shot/scene/angle fields. Metadata-based search. 1.6 added **metadata export/import** for teams working on duplicate copies of the same footage across geographies — a "reconnect/relink"-style merge workflow that Lesspain called "a game-changer in any dynamic production environment."

**Transcoding.** Batch transcode and lossless rewrap. Targets: H.264, ProRes (including **ProRes encoding on Windows**, added in 1.7 — a genuinely notable capability), Cineform, DNxHD/HR/HQ (Premium), MXF container writing (Premium). Burn-in timecode, apply LUTs and filters during conversion, denoise (advanced controls in Premium), audio channel mapping (Premium). Also "Combine" — assembly cuts of multiple clips without an NLE.

**Verified backup/offload.** Checksum-verified camera-card offload with **Media Hash List (MHL)** support, incremental backup (copies only what changed), up to 4 simultaneous destinations in Premium.

**Batch rename.** "Media-aware renaming engine" — rename driven by extracted technical metadata.

**NLE integration.** Send clips with metadata to Final Cut Pro, Premiere Pro, DaVinci Resolve, and Avid Media Composer (Avid arrived later, via ALE export + marker copy/paste). Delivery packages in Kyno Metadata, FCPX XML, FCP7/Premiere XML, and Excel formats. Premium adds integrated FTP/SFTP delivery with subclipping; enterprise adds Telestream Vantage, FileCatalyst, and Aspera.

## 3. The "no import" philosophy — how it worked and why users loved it

The mechanism: **no central database.** Metadata is written to hidden per-folder sidecar files — a `.LP_Store` directory alongside the media, containing undocumented `.lpmd` files named after each video ([NeoFinder documentation](https://www.cdfinder.de/guide/22/22.10/catalog_KYNO.html), which reverse-engineered the format).

Why users loved it, in order of how often it comes up:

1. **Portability.** "The best thing about Kyno is its portability. Metadata is stored in hidden sidecar files right alongside the media files. So if you've tagged a whole bunch of media, and then copy it to another drive, all the metadata gets copied too" (Lackey). Ship the drive, ship the logging.
2. **No imposed structure.** "It doesn't impose its own organizational system or requirements. It's entirely up to you how you want to work" (Lackey). Kyno markets this as "Non-intrusive — files remain where they are."
3. **Zero setup, zero infrastructure.** No server, no proxy storage tier, no DB admin. "Exceptionally easy to set up," "use instantly without training." A Larry Jordan reader called it "a low overhead low cost MAM alternative" for content produced daily ([larryjordan.com media management roundup](https://larryjordan.com/articles/video-editors-describe-how-they-manage-media-assets/)).
4. **Team sharing for free.** Because metadata lives on the storage, any Kyno client mounting the same NAS/SAN sees everyone's tags — "any metadata that any producer adds to an asset is available to others on the system" (same source). No sync layer required.
5. **Speed of the pre-edit loop.** Reviewing, culling, and subclipping without firing up an NLE and importing was the repeated refrain: "Having to fire up FCPX or Premiere to do the job is just too long a process."

## 4. What it lacked

**Offline search of disconnected drives — the single biggest structural gap.** "The biggest downside is there is no offline search. You can't search for media on a drive or in a location that is not mounted" (Lackey). Also impossible for the same architectural reason: **tracking a file's location as it moves across volumes and into archive.** Lackey is explicit that this is inherent to the sidecar design, not an oversight — "this kind of tracking is not possible without a centralised metadata database." This is precisely the niche NeoFinder, ClipCatalog, and Fast Video Cataloger sell against.

**No AI whatsoever.** No transcript/speech search, no visual or semantic search, no object/scene detection, no face or voice recognition. Every content-level tag is typed by a human. This is the near-universal framing of competitors in 2025–2026: "Kyno has no AI-powered search. You cannot type 'red car at sunset' and find matching clips. Tagging is manual" ([FrameQuery](https://www.framequery.com/blog/how-framequery-compares)); "Kyno depends on manual logging" ([Focus](https://use-focus.com/vs-kyno)); "No transcript-based search. Manual logging still required for content-level metadata" ([Reelback](https://www.reelback.io/blog/best-footage-logging-software-2026)).

**Other documented gaps:**
- **Search scope limits** — Drilldown is off by default and must be enabled to search a whole disk; you cannot search across multiple hard disks in one query; only media files are indexed, so folders, PDFs, and text documents never appear in results ([Larry Jordan first look](https://larryjordan.com/articles/first-look-kyno-media-management-software/)).
- **Subclips were not searchable** (2018 complaint, [Adobe forum](https://community.adobe.com/questions-729/prelude-and-kyno-alternatives-1346109)).
- **Metadata in a proprietary, undocumented format** rather than industry-standard XMP written into files/sidecars — a real lock-in risk that NeoFinder's rescue tool exists to solve.
- **No remote or browser access, no cloud collaboration** — "Desktop-only… Doesn't scale well for teams working remotely" (Reelback).
- **No workflow automation** — no watch folders or triggered pipelines, unlike a full MAM (Lackey).
- **Apple Silicon lag** — Rosetta-only and "often feels sluggish" on M-series Macs until Sept 2025 (Focus; fixed in 1.9).

## 5. Pricing/model history

Consistent perpetual-license-plus-annual-updates model, essentially unchanged from 2018 through today (prices in USD; EUR at parity, GBP ~£139/£319):

| Edition | Price | Renewal (1 more yr of updates) |
|---|---|---|
| Kyno Standard | $159 | $79/yr |
| Kyno Premium | $349 | $169/yr |
| Standard → Premium upgrade | $190 | — |
| Premium 5-seat team bundle | $1,570 | — |
| Academic Standard / Premium | $59 / $99 | — |

Source: [lesspain.software/kyno/buy/](https://lesspain.software/kyno/buy/).

**Model details that mattered to users:** the license is perpetual — "you can continue to use the version of Kyno that you have, for as long as you want" after the update year lapses — and bug-fix updates for your last version remain free even post-expiry. Single-user licenses activate on 2 machines, and cross-platform (Mac + Windows) on one license. Premium arrived at Kyno 1.5 (June 2018) with a 25% launch discount ($259). No free tier — "Try Kyno Free" on the homepage is a **30-day trial** (originally 14-day), with an additional 7-day trial granted per new minor version ([support article](https://support.lesspain.software/support/solutions/articles/12000068853-kyno-license-and-trial-activation)).

RedShark's Phil Rhodes made the notable observation that the perpetual model visibly drove development velocity: "a perpetual license gives a software house a more direct incentive to keep adding things, and Lesspain certainly have" ([RedShark, 2019](https://www.redsharknews.com/production/item/6752-lesspain-releases-kyno-1-7-and-it-makes-prores-workflows-practical-on-windows)) — which reads as ironic in hindsight given the post-acquisition stall. Also worth noting for competitive context: a Larry Jordan reader cited CatDV's shift to subscription with "fairly enterprise" pricing as a reason to look elsewhere — perpetual pricing was a differentiator in this segment.

## 6. Praise, complaints, and where users went

**Praise** — the press quotes Kyno itself features are representative and consistent across a decade: "the most full-featured media management system I've ever seen" (Larry Jordan); "quite possibly the single most useful piece of supplemental software for video post-production and media creation that I've ever used" (ProVideo Coalition); "A Swiss army knife of video handling" (RedShark); "Kyno has really transformed the way I work" (Philip Grossman); "In my opinion, Kyno is one of the best pieces of filmmaking software on the market. There is nothing else quite like it" (Matthew Allard). Named Software Product of the Year 2019. The recurring specific praises: Drilldown/flat view, format breadth, player quality on modest hardware, subclip-without-transcode speed, ease of learning, and metadata portability.

**Complaints:**
- **Thumbnail generation was slow and lazy-loaded.** "Only 1-2 thumbnails each second (ssd drive)… There is no way to generate all previews at once. It only loads the previews that show on your screen, that means you have to sit there and scroll through the files while the thumbnails are being created. If the program crashed all of them have to be reloaded" (Adobe forum, 2018). Partially addressed later — 1.9's release notes cite significantly improved thumbnail extraction with configurable CPU core use, and background pre-analyze.
- **Degrades with library scale.** Reports of sluggish search and batch tagging on large, deeply-nested high-res libraries ([Beverly Boy](https://beverlyboy.com/film-technology/raw-reality-kynos-limits-in-high-res-media-management/)).
- **Proprietary metadata storage** (see above).
- **The acquisition anxiety itself** was the dominant complaint 2021–2025. A DAM user's comment captures the specific fear: "the company was sold and I got worried it would be shortlived. (Something you do not want with DAM systems…)."
- **Redundancy for solo Resolve users.** A Blackmagic forum consensus held that "Kyno can't do very much for you if you have Resolve, a fast machine and are on your own" — though the same thread grants Kyno the edge for reviewing without loading all media, for collaborators on underpowered laptops, and for ad-hoc format conversion.

**Where ex-Kyno users went** (largely a 2021–2025 dormancy exodus):
- **NeoFinder** ($39.99, Mac) — the most direct, actively-courted migration path. Built a `.LP_Store`/`.lpmd` cataloger that maps Kyno ratings, keywords, title, description, camera/reel/shot/scene/angle and marker text into XMP, then can write it back to files as standard XMP. Its pitch is precisely Kyno's structural gap: **offline cataloging of disconnected drives**.
- **DaVinci Resolve's own media pool** — free, and covers metadata + subclipping for people already living in Resolve.
- **Silverstack (Pomfort)** — for the on-set DIT half of Kyno's use case; stronger RAW handling with GPU-accelerated R3D decode, built-in proxy generation, audio sync. Mac-only, project-based licensing ~$99–319 ([Tusk comparison](https://tuskbackup.com/blog/hedge-vs-shotput-pro-vs-silverstack)).
- **OffShoot (formerly Hedge) / ShotPut Pro** — for the verified-offload half ($169 one-time each).
- **axle.ai** — explicitly described as sharing "Axle AI's philosophy of lightweight, no-ingest media browsing," where "Kyno excels at transcoding and NLE metadata workflows but lacks the AI search and team collaboration features."
- **A wave of AI-native local-search challengers** positioning directly against Kyno by name, each with a dedicated "vs Kyno" landing page: **Focus** (native Rust, transcript + scene + people search), **FrameQuery**, **ClipCatalog** (Windows), **Reelback**, **Peakto**, **Fast Video Cataloger**. Their shared thesis — worth flagging as the competitive gap — is that Kyno's browse/transcode/offload/NLE-handoff layer is fine but its *retrieval* layer is a decade old.
- **CatDV** was the traditional step-up, but its move to subscription with enterprise pricing pushed some users back down-market.

**Net assessment:** Kyno's browse-without-import architecture, format breadth, verified offload, and transcode engine remain genuinely differentiated and are still endorsed as best-in-class for local/NAS DIT workflows as recently as 2026 ("If your team manages large local drives and a DIT workflow: Kyno remains one of the best desktop options" — Reelback). What has eroded is trust in its trajectory and its retrieval capability: four years of silence created a competitor field that now defines itself as "Kyno plus AI search," and 1.9 — a welcome maintenance release that also *removed* two integrations — has not yet answered that.
