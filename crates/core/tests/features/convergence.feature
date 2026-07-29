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

  # Amy's observation is deliberately first, giving amy and bob equal wall
  # time and counter (both machines' clocks start at the same fixed tick).
  # The Hlc total order (wall, counter, machine) then breaks the tie on
  # machine id, and "bob" > "amy" lexicographically — so bob's observation
  # is HLC-later without needing a second exchange or a different clock
  # value. That's a deterministic, minimal way to force an LWW winner.
  Scenario: Volume observations converge to the freshest label
    Given machine "amy" observes volume "V1" labeled "card-a"
    And machine "bob" observes volume "V1" labeled "card-a-renamed"
    When the machines exchange event logs
    Then both machines see volume "V1" labeled "card-a-renamed"

  # Bob's clock is poisoned far into the future before he tags "poison", so
  # his TagAdd event carries a wildly future Hlc. Amy's later TagRemove
  # still wins because OR-Set removal is causal (it cites the add's event
  # id) rather than LWW by timestamp — the poisoned clock cannot make its
  # own add un-removable, and amy's own clamp (24h ahead of her physical
  # now, per MAX_DRIFT_MS) keeps her subsequent HLCs from adopting the
  # poison outright.
  Scenario: A poisoned future clock cannot dominate ordering
    Given machine "amy" tags asset "A" with "good"
    And machine "bob" has a clock far in the future
    And machine "bob" tags asset "A" with "poison"
    And the machines exchange event logs
    When machine "amy" removes tag "poison" from asset "A"
    And the machines exchange event logs
    Then both machines see tags "good" on asset "A"
