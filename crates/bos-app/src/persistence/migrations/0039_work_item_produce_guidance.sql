-- Operator-authored context/guidance for the shared produce stage.
-- Stored on the work item so every packet kind generated from it receives the
-- same steer, through the revisioned work_queue mutation path.

ALTER TABLE work_items
ADD COLUMN produce_guidance TEXT NOT NULL DEFAULT '';
