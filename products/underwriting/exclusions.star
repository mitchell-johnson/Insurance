# The exclusion code list. A code names a clause. The core and every event
# carry the code only; the wording here is read under restricted access.
#
# Wording is placeholder text for development: the real clauses come from the
# underwriting and legal teams. Cause tags follow the claims module's
# taxonomy, and a tag catches itself and everything beneath it.
#
# EX-9xxx is reserved for verbatim clauses carried over from a legacy system.
# They are never matched mechanically: a claim on a cover that carries one
# always goes to an assessor with the original wording.

ALL = ["life", "trauma", "income_protection", "health"]
LIVING = ["trauma", "income_protection", "health"]

EXCLUSIONS = exclusion_library(
    id = "exclusions",
    version = 1,
    effective_from = "2026-01-01",
    entries = [
        # Musculoskeletal
        exclusion("EX-1001", 1, "body_system", LIVING, ["musculoskeletal.spine"], "Spine", "No benefit is payable for any disorder of the spine, its discs, nerves or supporting structures. Placeholder wording."),
        exclusion("EX-1002", 1, "body_system", LIVING, ["musculoskeletal.knee"], "Knee", "No benefit is payable for any disorder of the named knee. Placeholder wording."),
        exclusion("EX-1003", 1, "body_system", LIVING, ["musculoskeletal.shoulder"], "Shoulder", "No benefit is payable for any disorder of the named shoulder. Placeholder wording."),
        exclusion("EX-1004", 1, "body_system", LIVING, ["musculoskeletal.hip"], "Hip", "No benefit is payable for any disorder of the named hip. Placeholder wording."),
        exclusion("EX-1009", 1, "body_system", LIVING, ["musculoskeletal"], "Musculoskeletal system", "No benefit is payable for any musculoskeletal disorder. Placeholder wording."),
        # Mental health
        exclusion("EX-2001", 1, "mental_health", ["income_protection", "health"], ["mental"], "Mental health", "No benefit is payable for any mental health condition. Placeholder wording."),
        exclusion("EX-2002", 1, "mental_health", ["income_protection", "health"], ["mental.stress", "mental.anxiety"], "Stress and anxiety", "No benefit is payable for stress or anxiety conditions. Placeholder wording."),
        exclusion("EX-2003", 1, "mental_health", ALL, ["mental.substance"], "Substance use", "No benefit is payable for conditions arising from substance use. Placeholder wording."),
        exclusion("EX-2004", 1, "condition", ALL, ["injury.self_inflicted"], "Self-inflicted injury", "No benefit is payable for intentional self-inflicted injury. Placeholder wording."),
        # Cardiovascular and other systems
        exclusion("EX-3001", 1, "body_system", LIVING, ["cardiovascular.heart"], "Heart", "No benefit is payable for any disorder of the heart. Placeholder wording."),
        exclusion("EX-3002", 1, "body_system", LIVING, ["cardiovascular.stroke"], "Stroke", "No benefit is payable for stroke or cerebrovascular disease. Placeholder wording."),
        exclusion("EX-3009", 1, "body_system", LIVING, ["cardiovascular"], "Cardiovascular system", "No benefit is payable for any cardiovascular disorder. Placeholder wording."),
        exclusion("EX-3101", 1, "condition", LIVING, ["cancer"], "Cancer", "No benefit is payable for any cancer or its treatment. Placeholder wording."),
        exclusion("EX-3201", 1, "body_system", LIVING, ["neurological"], "Neurological system", "No benefit is payable for any neurological disorder. Placeholder wording."),
        exclusion("EX-3301", 1, "body_system", LIVING, ["respiratory"], "Respiratory system", "No benefit is payable for any respiratory disorder. Placeholder wording."),
        exclusion("EX-3401", 1, "body_system", LIVING, ["digestive"], "Digestive system", "No benefit is payable for any digestive disorder. Placeholder wording."),
        exclusion("EX-3501", 1, "body_system", LIVING, ["endocrine"], "Endocrine system", "No benefit is payable for any endocrine disorder, including diabetes and its complications. Placeholder wording."),
        exclusion("EX-3601", 1, "condition", ["income_protection", "health"], ["pregnancy"], "Pregnancy", "No benefit is payable for pregnancy, childbirth or their complications. Placeholder wording."),
        # Pursuits
        exclusion("EX-4001", 1, "pursuit", ALL, ["pursuit.aviation.private"], "Private aviation", "No benefit is payable for a claim arising from flying other than as a fare-paying passenger. Placeholder wording."),
        exclusion("EX-4002", 1, "pursuit", ALL, ["pursuit.motorsport"], "Motor sport", "No benefit is payable for a claim arising from motor sport. Placeholder wording."),
        exclusion("EX-4003", 1, "pursuit", ALL, ["pursuit.diving"], "Diving", "No benefit is payable for a claim arising from underwater diving. Placeholder wording."),
        exclusion("EX-4004", 1, "pursuit", ALL, ["pursuit.climbing"], "Climbing", "No benefit is payable for a claim arising from mountaineering or rock climbing. Placeholder wording."),
        exclusion("EX-4009", 1, "pursuit", ALL, ["pursuit"], "Hazardous pursuits", "No benefit is payable for a claim arising from any hazardous pursuit. Placeholder wording."),
        # Occupation, travel, pre-existing
        exclusion("EX-5001", 1, "occupation", ALL, ["occupation"], "Occupational duties", "No benefit is payable for a claim arising from the named occupational duties. Placeholder wording."),
        exclusion("EX-6001", 1, "travel", ALL, ["travel"], "Travel or residence abroad", "No benefit is payable for a claim arising while travelling or living outside the agreed countries. Placeholder wording."),
        exclusion("EX-7001", 1, "pre_existing", ["health", "income_protection"], ["pre_existing"], "Pre-existing conditions", "No benefit is payable for a condition whose signs or symptoms existed before the cover started. Placeholder wording."),
        # Verbatim clauses from a legacy system
        exclusion("EX-9000", 1, "verbatim", ALL + ["group_life"], [], "Verbatim legacy clause", "The clause is the original wording held in the document vault for this policy."),
    ],
)
