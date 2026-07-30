Feature: Verified multi-destination ingest

  Scenario: A card ingests verified to two destinations
    Given a source card with files
      | path        | bytes  |
      | clips/a.mov | AAAA   |
      | b.wav       | BBBBBB |
    And 2 destination roots
    When the card is ingested to "Projects/x/day1"
    Then every destination holds identical verified copies
    And every destination has an ASC MHL generation covering 2 files

  Scenario: A duplicate is skipped without copying
    Given a source card with files
      | path    | bytes |
      | dup.mov | AAAA  |
    And the catalog already knows content "AAAA"
    And 1 destination root
    When the card is ingested to "Projects/x/day1"
    Then no files are placed
    And 1 duplicate is reported

  Scenario: A corrupted write never reaches a final path
    Given a source card with files
      | path  | bytes |
      | a.mov | AAAA  |
    And 2 destination roots where destination 1 corrupts writes
    When the card is ingested to "Projects/x/day1"
    Then destination 1 reports a verification failure and holds only a quarantined partial
    And destination 2 holds an identical verified copy

  Scenario: An interrupted run resumes without re-copying placed files
    Given a source card with files
      | path  | bytes |
      | a.mov | AAAA  |
      | b.mov | BB    |
    And 1 destination root
    And a previous run already placed "a.mov"
    When the card is ingested to "Projects/x/day1"
    Then only "b.mov" is copied
