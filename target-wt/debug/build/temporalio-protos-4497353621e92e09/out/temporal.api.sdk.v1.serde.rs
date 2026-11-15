impl serde::Serialize for EventGroupMarker {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if self.variant.is_some() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.EventGroupMarker", len)?;
        if let Some(v) = self.variant.as_ref() {
            match v {
                event_group_marker::Variant::Label(v) => {
                    struct_ser.serialize_field("label", v)?;
                }
                event_group_marker::Variant::InboundEvent(v) => {
                    struct_ser.serialize_field("inboundEvent", v)?;
                }
                event_group_marker::Variant::InboundUpdate(v) => {
                    struct_ser.serialize_field("inboundUpdate", v)?;
                }
            }
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for EventGroupMarker {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "label",
            "inbound_event",
            "inboundEvent",
            "inbound_update",
            "inboundUpdate",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            Label,
            InboundEvent,
            InboundUpdate,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "label" => Ok(GeneratedField::Label),
                            "inboundEvent" | "inbound_event" => Ok(GeneratedField::InboundEvent),
                            "inboundUpdate" | "inbound_update" => Ok(GeneratedField::InboundUpdate),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = EventGroupMarker;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.EventGroupMarker")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<EventGroupMarker, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut variant__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::Label => {
                            if variant__.is_some() {
                                return Err(serde::de::Error::duplicate_field("label"));
                            }
                            variant__ = map_.next_value::<::std::option::Option<_>>()?.map(event_group_marker::Variant::Label)
;
                        }
                        GeneratedField::InboundEvent => {
                            if variant__.is_some() {
                                return Err(serde::de::Error::duplicate_field("inboundEvent"));
                            }
                            variant__ = map_.next_value::<::std::option::Option<_>>()?.map(event_group_marker::Variant::InboundEvent)
;
                        }
                        GeneratedField::InboundUpdate => {
                            if variant__.is_some() {
                                return Err(serde::de::Error::duplicate_field("inboundUpdate"));
                            }
                            variant__ = map_.next_value::<::std::option::Option<_>>()?.map(event_group_marker::Variant::InboundUpdate)
;
                        }
                    }
                }
                Ok(EventGroupMarker {
                    variant: variant__,
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.EventGroupMarker", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for event_group_marker::InboundEvent {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if self.inbound_event_id != 0 {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.EventGroupMarker.InboundEvent", len)?;
        if self.inbound_event_id != 0 {
            #[allow(clippy::needless_borrow)]
            #[allow(clippy::needless_borrows_for_generic_args)]
            struct_ser.serialize_field("inboundEventId", ToString::to_string(&self.inbound_event_id).as_str())?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for event_group_marker::InboundEvent {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "inbound_event_id",
            "inboundEventId",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            InboundEventId,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "inboundEventId" | "inbound_event_id" => Ok(GeneratedField::InboundEventId),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = event_group_marker::InboundEvent;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.EventGroupMarker.InboundEvent")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<event_group_marker::InboundEvent, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut inbound_event_id__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::InboundEventId => {
                            if inbound_event_id__.is_some() {
                                return Err(serde::de::Error::duplicate_field("inboundEventId"));
                            }
                            inbound_event_id__ = 
                                Some(map_.next_value::<::pbjson::private::NumberDeserialize<_>>()?.0)
                            ;
                        }
                    }
                }
                Ok(event_group_marker::InboundEvent {
                    inbound_event_id: inbound_event_id__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.EventGroupMarker.InboundEvent", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for event_group_marker::InboundUpdate {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if !self.inbound_update_id.is_empty() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.EventGroupMarker.InboundUpdate", len)?;
        if !self.inbound_update_id.is_empty() {
            struct_ser.serialize_field("inboundUpdateId", &self.inbound_update_id)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for event_group_marker::InboundUpdate {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "inbound_update_id",
            "inboundUpdateId",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            InboundUpdateId,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "inboundUpdateId" | "inbound_update_id" => Ok(GeneratedField::InboundUpdateId),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = event_group_marker::InboundUpdate;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.EventGroupMarker.InboundUpdate")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<event_group_marker::InboundUpdate, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut inbound_update_id__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::InboundUpdateId => {
                            if inbound_update_id__.is_some() {
                                return Err(serde::de::Error::duplicate_field("inboundUpdateId"));
                            }
                            inbound_update_id__ = Some(map_.next_value()?);
                        }
                    }
                }
                Ok(event_group_marker::InboundUpdate {
                    inbound_update_id: inbound_update_id__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.EventGroupMarker.InboundUpdate", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for event_group_marker::Label {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if !self.id.is_empty() {
            len += 1;
        }
        if self.label.is_some() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.EventGroupMarker.Label", len)?;
        if !self.id.is_empty() {
            struct_ser.serialize_field("id", &self.id)?;
        }
        if let Some(v) = self.label.as_ref() {
            struct_ser.serialize_field("label", v)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for event_group_marker::Label {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "id",
            "label",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            Id,
            Label,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "id" => Ok(GeneratedField::Id),
                            "label" => Ok(GeneratedField::Label),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = event_group_marker::Label;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.EventGroupMarker.Label")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<event_group_marker::Label, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut id__ = None;
                let mut label__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::Id => {
                            if id__.is_some() {
                                return Err(serde::de::Error::duplicate_field("id"));
                            }
                            id__ = Some(map_.next_value()?);
                        }
                        GeneratedField::Label => {
                            if label__.is_some() {
                                return Err(serde::de::Error::duplicate_field("label"));
                            }
                            label__ = map_.next_value()?;
                        }
                    }
                }
                Ok(event_group_marker::Label {
                    id: id__.unwrap_or_default(),
                    label: label__,
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.EventGroupMarker.Label", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for ExternalStorageReference {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if !self.driver_name.is_empty() {
            len += 1;
        }
        if !self.claim_data.is_empty() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.ExternalStorageReference", len)?;
        if !self.driver_name.is_empty() {
            struct_ser.serialize_field("driverName", &self.driver_name)?;
        }
        if !self.claim_data.is_empty() {
            struct_ser.serialize_field("claimData", &self.claim_data)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for ExternalStorageReference {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "driver_name",
            "driverName",
            "claim_data",
            "claimData",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            DriverName,
            ClaimData,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "driverName" | "driver_name" => Ok(GeneratedField::DriverName),
                            "claimData" | "claim_data" => Ok(GeneratedField::ClaimData),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = ExternalStorageReference;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.ExternalStorageReference")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<ExternalStorageReference, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut driver_name__ = None;
                let mut claim_data__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::DriverName => {
                            if driver_name__.is_some() {
                                return Err(serde::de::Error::duplicate_field("driverName"));
                            }
                            driver_name__ = Some(map_.next_value()?);
                        }
                        GeneratedField::ClaimData => {
                            if claim_data__.is_some() {
                                return Err(serde::de::Error::duplicate_field("claimData"));
                            }
                            claim_data__ = Some(
                                map_.next_value::<std::collections::HashMap<_, _>>()?
                            );
                        }
                    }
                }
                Ok(ExternalStorageReference {
                    driver_name: driver_name__.unwrap_or_default(),
                    claim_data: claim_data__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.ExternalStorageReference", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for UserMetadata {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if self.summary.is_some() {
            len += 1;
        }
        if self.details.is_some() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.UserMetadata", len)?;
        if let Some(v) = self.summary.as_ref() {
            struct_ser.serialize_field("summary", v)?;
        }
        if let Some(v) = self.details.as_ref() {
            struct_ser.serialize_field("details", v)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for UserMetadata {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "summary",
            "details",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            Summary,
            Details,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "summary" => Ok(GeneratedField::Summary),
                            "details" => Ok(GeneratedField::Details),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = UserMetadata;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.UserMetadata")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<UserMetadata, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut summary__ = None;
                let mut details__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::Summary => {
                            if summary__.is_some() {
                                return Err(serde::de::Error::duplicate_field("summary"));
                            }
                            summary__ = map_.next_value()?;
                        }
                        GeneratedField::Details => {
                            if details__.is_some() {
                                return Err(serde::de::Error::duplicate_field("details"));
                            }
                            details__ = map_.next_value()?;
                        }
                    }
                }
                Ok(UserMetadata {
                    summary: summary__,
                    details: details__,
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.UserMetadata", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for WorkerConfig {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if self.workflow_cache_size != 0 {
            len += 1;
        }
        if self.poller_behavior.is_some() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.WorkerConfig", len)?;
        if self.workflow_cache_size != 0 {
            struct_ser.serialize_field("workflowCacheSize", &self.workflow_cache_size)?;
        }
        if let Some(v) = self.poller_behavior.as_ref() {
            match v {
                worker_config::PollerBehavior::SimplePollerBehavior(v) => {
                    struct_ser.serialize_field("simplePollerBehavior", v)?;
                }
                worker_config::PollerBehavior::AutoscalingPollerBehavior(v) => {
                    struct_ser.serialize_field("autoscalingPollerBehavior", v)?;
                }
            }
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for WorkerConfig {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "workflow_cache_size",
            "workflowCacheSize",
            "simple_poller_behavior",
            "simplePollerBehavior",
            "autoscaling_poller_behavior",
            "autoscalingPollerBehavior",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            WorkflowCacheSize,
            SimplePollerBehavior,
            AutoscalingPollerBehavior,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "workflowCacheSize" | "workflow_cache_size" => Ok(GeneratedField::WorkflowCacheSize),
                            "simplePollerBehavior" | "simple_poller_behavior" => Ok(GeneratedField::SimplePollerBehavior),
                            "autoscalingPollerBehavior" | "autoscaling_poller_behavior" => Ok(GeneratedField::AutoscalingPollerBehavior),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = WorkerConfig;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.WorkerConfig")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<WorkerConfig, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut workflow_cache_size__ = None;
                let mut poller_behavior__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::WorkflowCacheSize => {
                            if workflow_cache_size__.is_some() {
                                return Err(serde::de::Error::duplicate_field("workflowCacheSize"));
                            }
                            workflow_cache_size__ = 
                                Some(map_.next_value::<::pbjson::private::NumberDeserialize<_>>()?.0)
                            ;
                        }
                        GeneratedField::SimplePollerBehavior => {
                            if poller_behavior__.is_some() {
                                return Err(serde::de::Error::duplicate_field("simplePollerBehavior"));
                            }
                            poller_behavior__ = map_.next_value::<::std::option::Option<_>>()?.map(worker_config::PollerBehavior::SimplePollerBehavior)
;
                        }
                        GeneratedField::AutoscalingPollerBehavior => {
                            if poller_behavior__.is_some() {
                                return Err(serde::de::Error::duplicate_field("autoscalingPollerBehavior"));
                            }
                            poller_behavior__ = map_.next_value::<::std::option::Option<_>>()?.map(worker_config::PollerBehavior::AutoscalingPollerBehavior)
;
                        }
                    }
                }
                Ok(WorkerConfig {
                    workflow_cache_size: workflow_cache_size__.unwrap_or_default(),
                    poller_behavior: poller_behavior__,
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.WorkerConfig", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for worker_config::AutoscalingPollerBehavior {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if self.min_pollers != 0 {
            len += 1;
        }
        if self.max_pollers != 0 {
            len += 1;
        }
        if self.initial_pollers != 0 {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.WorkerConfig.AutoscalingPollerBehavior", len)?;
        if self.min_pollers != 0 {
            struct_ser.serialize_field("minPollers", &self.min_pollers)?;
        }
        if self.max_pollers != 0 {
            struct_ser.serialize_field("maxPollers", &self.max_pollers)?;
        }
        if self.initial_pollers != 0 {
            struct_ser.serialize_field("initialPollers", &self.initial_pollers)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for worker_config::AutoscalingPollerBehavior {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "min_pollers",
            "minPollers",
            "max_pollers",
            "maxPollers",
            "initial_pollers",
            "initialPollers",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            MinPollers,
            MaxPollers,
            InitialPollers,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "minPollers" | "min_pollers" => Ok(GeneratedField::MinPollers),
                            "maxPollers" | "max_pollers" => Ok(GeneratedField::MaxPollers),
                            "initialPollers" | "initial_pollers" => Ok(GeneratedField::InitialPollers),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = worker_config::AutoscalingPollerBehavior;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.WorkerConfig.AutoscalingPollerBehavior")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<worker_config::AutoscalingPollerBehavior, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut min_pollers__ = None;
                let mut max_pollers__ = None;
                let mut initial_pollers__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::MinPollers => {
                            if min_pollers__.is_some() {
                                return Err(serde::de::Error::duplicate_field("minPollers"));
                            }
                            min_pollers__ = 
                                Some(map_.next_value::<::pbjson::private::NumberDeserialize<_>>()?.0)
                            ;
                        }
                        GeneratedField::MaxPollers => {
                            if max_pollers__.is_some() {
                                return Err(serde::de::Error::duplicate_field("maxPollers"));
                            }
                            max_pollers__ = 
                                Some(map_.next_value::<::pbjson::private::NumberDeserialize<_>>()?.0)
                            ;
                        }
                        GeneratedField::InitialPollers => {
                            if initial_pollers__.is_some() {
                                return Err(serde::de::Error::duplicate_field("initialPollers"));
                            }
                            initial_pollers__ = 
                                Some(map_.next_value::<::pbjson::private::NumberDeserialize<_>>()?.0)
                            ;
                        }
                    }
                }
                Ok(worker_config::AutoscalingPollerBehavior {
                    min_pollers: min_pollers__.unwrap_or_default(),
                    max_pollers: max_pollers__.unwrap_or_default(),
                    initial_pollers: initial_pollers__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.WorkerConfig.AutoscalingPollerBehavior", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for worker_config::SimplePollerBehavior {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if self.max_pollers != 0 {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.WorkerConfig.SimplePollerBehavior", len)?;
        if self.max_pollers != 0 {
            struct_ser.serialize_field("maxPollers", &self.max_pollers)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for worker_config::SimplePollerBehavior {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "max_pollers",
            "maxPollers",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            MaxPollers,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "maxPollers" | "max_pollers" => Ok(GeneratedField::MaxPollers),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = worker_config::SimplePollerBehavior;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.WorkerConfig.SimplePollerBehavior")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<worker_config::SimplePollerBehavior, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut max_pollers__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::MaxPollers => {
                            if max_pollers__.is_some() {
                                return Err(serde::de::Error::duplicate_field("maxPollers"));
                            }
                            max_pollers__ = 
                                Some(map_.next_value::<::pbjson::private::NumberDeserialize<_>>()?.0)
                            ;
                        }
                    }
                }
                Ok(worker_config::SimplePollerBehavior {
                    max_pollers: max_pollers__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.WorkerConfig.SimplePollerBehavior", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for WorkflowDefinition {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if !self.r#type.is_empty() {
            len += 1;
        }
        if !self.query_definitions.is_empty() {
            len += 1;
        }
        if !self.signal_definitions.is_empty() {
            len += 1;
        }
        if !self.update_definitions.is_empty() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.WorkflowDefinition", len)?;
        if !self.r#type.is_empty() {
            struct_ser.serialize_field("type", &self.r#type)?;
        }
        if !self.query_definitions.is_empty() {
            struct_ser.serialize_field("queryDefinitions", &self.query_definitions)?;
        }
        if !self.signal_definitions.is_empty() {
            struct_ser.serialize_field("signalDefinitions", &self.signal_definitions)?;
        }
        if !self.update_definitions.is_empty() {
            struct_ser.serialize_field("updateDefinitions", &self.update_definitions)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for WorkflowDefinition {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "type",
            "query_definitions",
            "queryDefinitions",
            "signal_definitions",
            "signalDefinitions",
            "update_definitions",
            "updateDefinitions",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            Type,
            QueryDefinitions,
            SignalDefinitions,
            UpdateDefinitions,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "type" => Ok(GeneratedField::Type),
                            "queryDefinitions" | "query_definitions" => Ok(GeneratedField::QueryDefinitions),
                            "signalDefinitions" | "signal_definitions" => Ok(GeneratedField::SignalDefinitions),
                            "updateDefinitions" | "update_definitions" => Ok(GeneratedField::UpdateDefinitions),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = WorkflowDefinition;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.WorkflowDefinition")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<WorkflowDefinition, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut r#type__ = None;
                let mut query_definitions__ = None;
                let mut signal_definitions__ = None;
                let mut update_definitions__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::Type => {
                            if r#type__.is_some() {
                                return Err(serde::de::Error::duplicate_field("type"));
                            }
                            r#type__ = Some(map_.next_value()?);
                        }
                        GeneratedField::QueryDefinitions => {
                            if query_definitions__.is_some() {
                                return Err(serde::de::Error::duplicate_field("queryDefinitions"));
                            }
                            query_definitions__ = Some(map_.next_value()?);
                        }
                        GeneratedField::SignalDefinitions => {
                            if signal_definitions__.is_some() {
                                return Err(serde::de::Error::duplicate_field("signalDefinitions"));
                            }
                            signal_definitions__ = Some(map_.next_value()?);
                        }
                        GeneratedField::UpdateDefinitions => {
                            if update_definitions__.is_some() {
                                return Err(serde::de::Error::duplicate_field("updateDefinitions"));
                            }
                            update_definitions__ = Some(map_.next_value()?);
                        }
                    }
                }
                Ok(WorkflowDefinition {
                    r#type: r#type__.unwrap_or_default(),
                    query_definitions: query_definitions__.unwrap_or_default(),
                    signal_definitions: signal_definitions__.unwrap_or_default(),
                    update_definitions: update_definitions__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.WorkflowDefinition", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for WorkflowInteractionDefinition {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if !self.name.is_empty() {
            len += 1;
        }
        if !self.description.is_empty() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.WorkflowInteractionDefinition", len)?;
        if !self.name.is_empty() {
            struct_ser.serialize_field("name", &self.name)?;
        }
        if !self.description.is_empty() {
            struct_ser.serialize_field("description", &self.description)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for WorkflowInteractionDefinition {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "name",
            "description",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            Name,
            Description,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "name" => Ok(GeneratedField::Name),
                            "description" => Ok(GeneratedField::Description),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = WorkflowInteractionDefinition;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.WorkflowInteractionDefinition")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<WorkflowInteractionDefinition, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut name__ = None;
                let mut description__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::Name => {
                            if name__.is_some() {
                                return Err(serde::de::Error::duplicate_field("name"));
                            }
                            name__ = Some(map_.next_value()?);
                        }
                        GeneratedField::Description => {
                            if description__.is_some() {
                                return Err(serde::de::Error::duplicate_field("description"));
                            }
                            description__ = Some(map_.next_value()?);
                        }
                    }
                }
                Ok(WorkflowInteractionDefinition {
                    name: name__.unwrap_or_default(),
                    description: description__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.WorkflowInteractionDefinition", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for WorkflowMetadata {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if self.definition.is_some() {
            len += 1;
        }
        if !self.current_details.is_empty() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.WorkflowMetadata", len)?;
        if let Some(v) = self.definition.as_ref() {
            struct_ser.serialize_field("definition", v)?;
        }
        if !self.current_details.is_empty() {
            struct_ser.serialize_field("currentDetails", &self.current_details)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for WorkflowMetadata {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "definition",
            "current_details",
            "currentDetails",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            Definition,
            CurrentDetails,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "definition" => Ok(GeneratedField::Definition),
                            "currentDetails" | "current_details" => Ok(GeneratedField::CurrentDetails),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = WorkflowMetadata;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.WorkflowMetadata")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<WorkflowMetadata, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut definition__ = None;
                let mut current_details__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::Definition => {
                            if definition__.is_some() {
                                return Err(serde::de::Error::duplicate_field("definition"));
                            }
                            definition__ = map_.next_value()?;
                        }
                        GeneratedField::CurrentDetails => {
                            if current_details__.is_some() {
                                return Err(serde::de::Error::duplicate_field("currentDetails"));
                            }
                            current_details__ = Some(map_.next_value()?);
                        }
                    }
                }
                Ok(WorkflowMetadata {
                    definition: definition__,
                    current_details: current_details__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.WorkflowMetadata", FIELDS, GeneratedVisitor)
    }
}
impl serde::Serialize for WorkflowTaskCompletedMetadata {
    #[allow(deprecated)]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut len = 0;
        if !self.core_used_flags.is_empty() {
            len += 1;
        }
        if !self.lang_used_flags.is_empty() {
            len += 1;
        }
        if !self.sdk_name.is_empty() {
            len += 1;
        }
        if !self.sdk_version.is_empty() {
            len += 1;
        }
        let mut struct_ser = serializer.serialize_struct("temporal.api.sdk.v1.WorkflowTaskCompletedMetadata", len)?;
        if !self.core_used_flags.is_empty() {
            struct_ser.serialize_field("coreUsedFlags", &self.core_used_flags)?;
        }
        if !self.lang_used_flags.is_empty() {
            struct_ser.serialize_field("langUsedFlags", &self.lang_used_flags)?;
        }
        if !self.sdk_name.is_empty() {
            struct_ser.serialize_field("sdkName", &self.sdk_name)?;
        }
        if !self.sdk_version.is_empty() {
            struct_ser.serialize_field("sdkVersion", &self.sdk_version)?;
        }
        struct_ser.end()
    }
}
impl<'de> serde::Deserialize<'de> for WorkflowTaskCompletedMetadata {
    #[allow(deprecated)]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const FIELDS: &[&str] = &[
            "core_used_flags",
            "coreUsedFlags",
            "lang_used_flags",
            "langUsedFlags",
            "sdk_name",
            "sdkName",
            "sdk_version",
            "sdkVersion",
        ];

        #[allow(clippy::enum_variant_names)]
        enum GeneratedField {
            CoreUsedFlags,
            LangUsedFlags,
            SdkName,
            SdkVersion,
        }
        impl<'de> serde::Deserialize<'de> for GeneratedField {
            fn deserialize<D>(deserializer: D) -> std::result::Result<GeneratedField, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct GeneratedVisitor;

                impl serde::de::Visitor<'_> for GeneratedVisitor {
                    type Value = GeneratedField;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(formatter, "expected one of: {:?}", &FIELDS)
                    }

                    #[allow(unused_variables)]
                    fn visit_str<E>(self, value: &str) -> std::result::Result<GeneratedField, E>
                    where
                        E: serde::de::Error,
                    {
                        match value {
                            "coreUsedFlags" | "core_used_flags" => Ok(GeneratedField::CoreUsedFlags),
                            "langUsedFlags" | "lang_used_flags" => Ok(GeneratedField::LangUsedFlags),
                            "sdkName" | "sdk_name" => Ok(GeneratedField::SdkName),
                            "sdkVersion" | "sdk_version" => Ok(GeneratedField::SdkVersion),
                            _ => Err(serde::de::Error::unknown_field(value, FIELDS)),
                        }
                    }
                }
                deserializer.deserialize_identifier(GeneratedVisitor)
            }
        }
        struct GeneratedVisitor;
        impl<'de> serde::de::Visitor<'de> for GeneratedVisitor {
            type Value = WorkflowTaskCompletedMetadata;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct temporal.api.sdk.v1.WorkflowTaskCompletedMetadata")
            }

            fn visit_map<V>(self, mut map_: V) -> std::result::Result<WorkflowTaskCompletedMetadata, V::Error>
                where
                    V: serde::de::MapAccess<'de>,
            {
                let mut core_used_flags__ = None;
                let mut lang_used_flags__ = None;
                let mut sdk_name__ = None;
                let mut sdk_version__ = None;
                while let Some(k) = map_.next_key()? {
                    match k {
                        GeneratedField::CoreUsedFlags => {
                            if core_used_flags__.is_some() {
                                return Err(serde::de::Error::duplicate_field("coreUsedFlags"));
                            }
                            core_used_flags__ = 
                                Some(map_.next_value::<Vec<::pbjson::private::NumberDeserialize<_>>>()?
                                    .into_iter().map(|x| x.0).collect())
                            ;
                        }
                        GeneratedField::LangUsedFlags => {
                            if lang_used_flags__.is_some() {
                                return Err(serde::de::Error::duplicate_field("langUsedFlags"));
                            }
                            lang_used_flags__ = 
                                Some(map_.next_value::<Vec<::pbjson::private::NumberDeserialize<_>>>()?
                                    .into_iter().map(|x| x.0).collect())
                            ;
                        }
                        GeneratedField::SdkName => {
                            if sdk_name__.is_some() {
                                return Err(serde::de::Error::duplicate_field("sdkName"));
                            }
                            sdk_name__ = Some(map_.next_value()?);
                        }
                        GeneratedField::SdkVersion => {
                            if sdk_version__.is_some() {
                                return Err(serde::de::Error::duplicate_field("sdkVersion"));
                            }
                            sdk_version__ = Some(map_.next_value()?);
                        }
                    }
                }
                Ok(WorkflowTaskCompletedMetadata {
                    core_used_flags: core_used_flags__.unwrap_or_default(),
                    lang_used_flags: lang_used_flags__.unwrap_or_default(),
                    sdk_name: sdk_name__.unwrap_or_default(),
                    sdk_version: sdk_version__.unwrap_or_default(),
                })
            }
        }
        deserializer.deserialize_struct("temporal.api.sdk.v1.WorkflowTaskCompletedMetadata", FIELDS, GeneratedVisitor)
    }
}
