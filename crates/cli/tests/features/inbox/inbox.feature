Feature: Inbox contributions
  A shared drop folder becomes cataloged, verified, provenance-tagged media.

  Scenario: A manifested contribution is ingested with provenance
    Given a catalog with a PARA project "spring"
    And a contribution "drop-1" of 2 files from contributor "dana" targeting "project/spring"
    When I process the inbox
    Then the report says "drop-1" was ingested with 2 files
    And searching "tag:contributor/dana" finds every tracked file
    And the contribution folder has moved to ".processed"

  Scenario: An incomplete upload waits and converges
    Given a catalog with a PARA project "spring"
    And a contribution "drop-2" whose manifest promises a file that is short on disk
    When I process the inbox
    Then the report says "drop-2" is waiting
    When the file finishes uploading
    And I process the inbox
    Then the report says "drop-2" was ingested with 1 files

  Scenario: A hash mismatch is recorded once and skipped after
    Given a catalog with a PARA project "spring"
    And a contribution "drop-3" whose manifest hash does not match the file
    When I process the inbox expecting failure
    Then the report names the mismatched file and both hashes
    When I process the inbox
    Then the report says "drop-3" was skipped with a recorded failure

  Scenario: An unknown manifest version is skipped with a named remedy
    Given a catalog with a PARA project "spring"
    And a contribution "drop-4" with manifest version 99
    When I process the inbox expecting failure
    Then the report names version 99 and the supported version 1

  Scenario: Manifest-less drops triage after quiescence
    Given a catalog with a PARA resource "inbox-triage"
    And a quiescent manifest-less folder "beach" holding 1 file
    When I process the inbox with triage target "resource/inbox-triage"
    Then searching "tag:source/inbox" finds the file
