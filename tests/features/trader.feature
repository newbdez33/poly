Feature: Martingale 5-minute trader
  As bot operator, I want disciplined Martingale execution per 5-min window.

  Background:
    Given direction "UP", base $5, max_step 5
    And trader has fresh ladder state

  Scenario: First window won
    When the trader records a win paying $9.90 on a $5 bet
    Then ladder step is 1
    And realized_pnl is $4.90

  Scenario: Cap reached after 5 losses
    Given ladder at step 5
    When the trader records a loss of $80
    Then session_stopped is CapReached

  Scenario: Loss advances ladder
    Given ladder at step 2
    When the trader records a loss of $10
    Then ladder step is 3
    And realized_pnl is $-10

  Scenario: Skip does not change ladder
    Given ladder at step 3
    When the trader records a skipped window
    Then ladder step is 3

  Scenario: Win after losses resets ladder
    Given ladder at step 4
    When the trader records a win paying $79.20 on a $40 bet
    Then ladder step is 1

  Scenario: Cumulative loss to cap
    When the trader loses 5 windows in a row
    Then session_stopped is CapReached
    And realized_pnl is $-155
