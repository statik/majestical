Feature: Two machines converge through any exchange of event logs
  The catalog is a projection of merged event logs. Whatever order logs
  arrive in, and however often they are replayed, both machines see the
  same catalog.

  Scenario: Concurrent tagging merges as a union
    Given machine "amy" tags asset "A" with "topic/drone"
    And machine "bob" tags asset "A" with "status/select"
    When the machines exchange event logs
    Then both machines see tags "status/select, topic/drone" on asset "A"

  Scenario: A re-add unseen by any remove survives concurrent removes
    Given machine "amy" tags asset "A" with "keep"
    And the machines exchange event logs
    And machine "bob" removes tag "keep" from asset "A"
    And machine "amy" removes tag "keep" from asset "A"
    And machine "amy" tags asset "A" with "keep"
    When the machines exchange event logs
    Then both machines see tags "keep" on asset "A"

  Scenario: Replaying the same log twice changes nothing
    Given machine "amy" tags asset "A" with "topic/drone"
    When the machines exchange event logs
    And the machines exchange event logs
    Then both machines see tags "topic/drone" on asset "A"
