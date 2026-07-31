Feature: Layered search
  Search must answer from whatever layers exist: filters and name matching
  always work; the semantic layer joins when the model and index exist;
  offline volumes stay searchable.

  Scenario: Name search with a tag filter and negation
    Given a catalog with assets "beach_day.mov" and "mountain.jpg"
    And "beach_day.mov" is tagged "status/select"
    When I search "beach tag:status/select"
    Then the results contain "beach_day.mov"
    When I search "beach -tag:status/select"
    Then the results are empty

  Scenario: Search without the encoder model degrades with a notice
    Given a catalog with assets "beach_day.mov" and "mountain.jpg"
    And no encoder model is installed
    When I search "beach"
    Then the results contain "beach_day.mov"
    And the notice mentions "maj model fetch"

  Scenario: Saved searches round-trip between machines
    Given a catalog with assets "beach_day.mov" and "mountain.jpg"
    When machine "a" saves the search "tag:keep" as "keepers"
    Then machine "b" lists a saved search named "keepers"

  Scenario: A kind filter selects by media class
    Given a catalog with assets "beach_day.mov" and "mountain.jpg"
    When I search "kind:image"
    Then the results contain "mountain.jpg"
    And the results do not contain "beach_day.mov"
