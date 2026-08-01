Feature: Layered text search
  Transcript text becomes searchable once the derivation queue heals a
  transcript blob into text_fts: hits carry a timestamp and a snippet,
  `in:` scopes where terms match, hard filters still intersect text hits,
  and a missing encoder model degrades with a notice naming the gap —
  all without fetching any model.

  Scenario: Transcript text is searchable after indexing
    Given a catalog with a scanned audio file "standup.wav"
    And a transcript blob containing "quarterly budget review"
    When I run index with kinds "transcripts"
    And I search "quarterly budget"
    Then the results contain "standup.wav"
    And the hit for "standup.wav" shows a timestamp and a snippet

  Scenario: Source filter restricts where terms match
    Given a catalog with a scanned audio file "standup.wav"
    And a transcript blob containing "quarterly budget review"
    When I run index with kinds "transcripts"
    And I search "quarterly in:ocr"
    Then the results are empty
    When I search "quarterly in:transcript"
    Then the results contain "standup.wav"

  Scenario: Hard filters intersect text hits
    Given a catalog with a scanned audio file "standup.wav"
    And a transcript blob containing "quarterly budget review"
    When I run index with kinds "transcripts"
    And I search "quarterly tag:missing"
    Then the results are empty

  Scenario: Missing models degrade with a named gap
    Given a catalog with a scanned audio file "standup.wav"
    And a transcript blob containing "quarterly budget review"
    When I search "quarterly budget"
    Then the notice mentions "model fetch --only minilm-l6-v2-v1"
