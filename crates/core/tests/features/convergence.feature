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

  # Bob observes first, so his event stamps (1,0,bob). After the first
  # exchange amy adopts that timestamp into her own clock, so her own
  # observation stamps (1,1,amy) — genuinely later by (wall, counter)
  # despite "amy" sorting before "bob". This deliberately works against the
  # machine-id tiebreak: a bug that picked the LWW winner by comparing
  # machine ids instead of the full Hlc would pick bob's, not amy's, and
  # this scenario would fail. The tiebreak case itself (equal wall and
  # counter) is covered by the property tests, not here.
  Scenario: Volume observations converge to the freshest label
    Given machine "bob" observes volume "V1" labeled "card-a"
    And the machines exchange event logs
    And machine "amy" observes volume "V1" labeled "card-a-renamed"
    When the machines exchange event logs
    Then both machines see volume "V1" labeled "card-a-renamed"

  # Bob's clock is poisoned far into the future before he tags "poison", so
  # his TagAdd event carries a wildly future Hlc. Amy's later TagRemove
  # still wins because OR-Set removal is causal (it cites the add's event
  # id) rather than LWW by timestamp — the poisoned clock cannot make its
  # own add un-removable, and amy's own clamp (bounded drift, per
  # MAX_DRIFT_MS, ahead of her physical now) keeps her subsequent HLCs from
  # adopting the poison outright.
  Scenario: A poisoned future clock cannot dominate ordering
    Given machine "amy" tags asset "A" with "good"
    And machine "bob" has a clock far in the future
    And machine "bob" tags asset "A" with "poison"
    And the machines exchange event logs
    When machine "amy" removes tag "poison" from asset "A"
    And the machines exchange event logs
    Then both machines see tags "good" on asset "A"
    And machine "amy" clamped a far-future timestamp
