---------------------------- MODULE BatonCiAdmission ----------------------------
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Gates,
          TreatSkippedAsSuccess,
          MergeUsesSelection,
          RetryMasksFailure,
          UnknownSelectsNone,
          CommandDriftAllowed,
          ReportWithoutStart,
          CommandsDrifted,
          InventoryWithoutTest

Events == {"pull_request", "push", "merge_group", "workflow_dispatch", "unknown"}
Results == {"pending", "success", "failure", "skipped"}
SkipCauses == {"none", "unselected", "selected"}

VARIABLES event, selected, started, result, skipCause, verify, commandsMatch

vars == <<event, selected, started, result, skipCause, verify, commandsMatch>>

SelectedFor(e) ==
  LET base == [g \in Gates |->
        CASE e = "unknown" -> ~UnknownSelectsNone
          [] e = "merge_group" -> ~MergeUsesSelection
          [] OTHER -> TRUE]
  IN [g \in Gates |->
        IF g = "inventory"
        THEN IF InventoryWithoutTest THEN TRUE ELSE base["test"]
        ELSE base[g]]

TypeOK ==
  /\ event \in Events
  /\ selected \in [Gates -> BOOLEAN]
  /\ started \in [Gates -> BOOLEAN]
  /\ result \in [Gates -> Results]
  /\ skipCause \in [Gates -> SkipCauses]
  /\ verify \in {"pending", "success", "failure"}
  /\ commandsMatch = ~CommandsDrifted

Init ==
  /\ event \in Events
  /\ commandsMatch = ~CommandsDrifted
  /\ selected = SelectedFor(event)
  /\ started = [g \in Gates |-> FALSE]
  /\ result = [g \in Gates |-> "pending"]
  /\ skipCause = [g \in Gates |-> "none"]
  /\ verify = "pending"

Report(g) ==
  /\ result[g] = "pending"
  /\ selected[g]
  /\ \E outcome \in {"success", "failure"}:
       /\ result' = [result EXCEPT ![g] = outcome]
       /\ started' = [started EXCEPT ![g] = IF ReportWithoutStart THEN started[g] ELSE TRUE]
       /\ UNCHANGED <<event, selected, skipCause, verify, commandsMatch>>

Skip(g, cause) ==
  /\ result[g] = "pending"
  /\ cause \in {"unselected", "selected"}
  /\ (cause = "unselected" => ~selected[g])
  /\ (cause = "selected" => selected[g])
  /\ result' = [result EXCEPT ![g] = "skipped"]
  /\ skipCause' = [skipCause EXCEPT ![g] = cause]
  /\ UNCHANGED <<event, selected, started, verify, commandsMatch>>

AllTerminal == \A g \in Gates: result[g] # "pending"

FinishVerify ==
  /\ verify = "pending"
  /\ AllTerminal
  /\ LET
       core == /\ result["changes"] = "success"
               /\ result["selection"] = "success"
               /\ result["workflow_guard"] = "success"
       selectedOk == \A g \in Gates:
         selected[g] =>
           \/ /\ result[g] = "success"
              /\ (started[g] \/ ReportWithoutStart)
           \/ (TreatSkippedAsSuccess /\ result[g] = "skipped")
           \/ (RetryMasksFailure /\ result[g] = "failure")
       skippedOk == \A g \in Gates:
         result[g] = "skipped" => (skipCause[g] = "unselected" \/ TreatSkippedAsSuccess)
       commandOk == commandsMatch \/ CommandDriftAllowed
       ok == core /\ selectedOk /\ skippedOk /\ commandOk
     IN verify' = IF ok THEN "success" ELSE "failure"
  /\ UNCHANGED <<event, selected, started, result, skipCause, commandsMatch>>

Done ==
  /\ verify # "pending"
  /\ UNCHANGED vars

Next == \E g \in Gates:
  Report(g) \/ Skip(g, "unselected") \/ Skip(g, "selected") \/ FinishVerify \/ Done

Spec == Init /\ [][Next]_vars

NoFalseGreen ==
  verify = "success" =>
    /\ result["changes"] = "success"
    /\ result["selection"] = "success"
    /\ result["workflow_guard"] = "success"
    /\ \A g \in Gates: selected[g] => /\ result[g] = "success" /\ started[g]
    /\ \A g \in Gates: result[g] = "skipped" => skipCause[g] = "unselected"

FullSelection ==
  event \in {"push", "merge_group", "workflow_dispatch"} => \A g \in Gates: selected[g]

NoRetryMasking ==
  verify = "success" => \A g \in Gates: result[g] # "failure"

ChangesFailOpen ==
  event = "unknown" => \A g \in Gates: selected[g]

InventoryNeedsTest ==
  selected["inventory"] => selected["test"]

CommandsMatch ==
  verify = "success" => commandsMatch

================================================================================
