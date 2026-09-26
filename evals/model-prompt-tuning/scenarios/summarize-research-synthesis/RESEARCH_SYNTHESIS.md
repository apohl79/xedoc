# Research synthesis: reliable critical-message delivery

## Question

The system delivers critical conversation messages to several independent
consumers. A prior design treated successful publication to the message broker
as success. The research examined whether that signal is sufficient for a
zero-unconfirmed-delivery objective during consumer restarts, routing changes,
and partial outages.

## Evidence from the system

Three distinct loss classes emerged.

1. **Publication loss:** the producer never receives confirmation that the
   broker accepted the message.
2. **Coverage loss:** the broker accepts the message, but a consumer that
   should receive it is absent, unbound, or unreachable.
3. **Completion loss:** a consumer receives a message but never confirms its
   work completed, perhaps because it crashes after receipt.

Broker acknowledgement addresses only publication loss. It cannot prove that
the expected consumers existed at publication time or that each consumer
completed its work. Draining a consumer before a redeploy reduces disruption,
but it is not evidence of redelivery or completed work after the process
stops.

## Identity and ownership

The investigation separated two concepts that the old design conflated.
Conversation lifecycle ownership determines who may end, hold, or resume a
conversation. Delivery addressing determines which bridge or consumer should
receive one outbound message. The lifecycle owner is therefore not necessarily
the delivery target.

The reliable delivery key must include the publication identifier, the target
scope, and the consumer role. A broker subject alone is unsuitable: subjects
can be broader than a delivery target and can remain stable while a participant
creates a new delivery leg. Keying receipts by subject would count the wrong
number of obligations.

Producers provide only stable routing facts such as tenant, conversation, and
target legs. Policy decides which consumer roles are required. Consumers bind
their roles when a leg joins. This allows the expected consumer set to be
determined at publication time without forcing the producer to know concrete
process instances.

## Design implications

A zero-unconfirmed objective requires a durable intent record for every
expected delivery. The record tracks the publication, expected roles, and
confirmed outcomes. An in-memory counter or a transport acknowledgement cannot
survive restart and cannot distinguish an acknowledged message from a completed
obligation.

The likely integration point is a message-bus wrapper rather than each
application. A wrapper can record delivery intent before publishing, observe
consumer binding and receipt events, and expose the same interface to existing
callers. It also keeps tests using the existing message-bus mock. A shared
ledger is required; the investigated deployment does not have a general shared
cache that every consumer can rely on.

Reporting must separate three states: full coverage, degraded coverage, and
unknown coverage. A window with missing coverage cannot be reported as clean
merely because no completion failures were observed. The proposed service-level
objective is zero unconfirmed deliveries during full-coverage windows, paired
with a separate coverage indicator and alert.

## Recommendation and unresolved work

Start with a narrow, user-approved deployment: record decisions, expected
consumer roles, applications, and observed outcomes; do not claim savings from
counterfactual routing or add automatic recovery policy yet. Stage the work as
ledger provisioning, wrapper implementation, consumer integration, and drills
that demonstrate restart and partial-outage behavior.

Two decisions remain open. First, projected message volume and fan-out are not
measured, so ledger sizing and cost cannot be justified. Second, degraded-mode
thresholds and the accepted recovery-point objective need product ownership.
Those gaps block final sizing and cannot be solved by changing the message
schema alone.
