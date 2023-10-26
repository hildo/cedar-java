/*
 * Copyright 2022-2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use cedar_policy::{
    frontend::{
        is_authorized::json_is_authorized, utils::InterfaceResult, validate::json_validate,
    },
    EntityUid,
};
use jni::{
    objects::{JClass, JObject, JString, JValueGen, JValueOwned},
    sys::{jstring, jvalue},
    JNIEnv,
};
use jni_fn::jni_fn;
use serde::{Deserialize, Serialize};
use std::{error::Error, str::FromStr, thread};

use crate::{
    objects::{JEntityId, JEntityTypeName, JEntityUID, Object},
    utils::raise_npe,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const V0_AUTH_OP: &str = "AuthorizationOperation";
const V0_VALIDATE_OP: &str = "ValidateOperation";
const V0_PARSE_EUID_OP: &str = "ParseEntityUidOperation";

fn build_err_obj(env: &JNIEnv<'_>, err: &str) -> jstring {
    env.new_string(
        serde_json::to_string(&InterfaceResult::fail_bad_request(vec![format!(
            "Failed {} Java string",
            err
        )]))
        .expect("could not serialise response"),
    )
    .expect("error creating Java string")
    .into_raw()
}

fn call_cedar_in_thread(call_str: String, input_str: String) -> String {
    call_cedar(&call_str, &input_str)
}

/// The main JNI entry point
#[jni_fn("com.cedarpolicy.BasicAuthorizationEngine")]
pub fn callCedarJNI(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    j_call: JString<'_>,
    j_input: JString<'_>,
) -> jstring {
    let j_call_str: String = match env.get_string(&j_call) {
        Ok(call_str) => call_str.into(),
        _ => return build_err_obj(&env, "getting"),
    };

    let mut j_input_str: String = match env.get_string(&j_input) {
        Ok(s) => s.into(),
        Err(_) => return build_err_obj(&env, "parsing"),
    };
    j_input_str.push(' ');

    let handle = thread::spawn(move || call_cedar_in_thread(j_call_str, j_input_str));

    let result = match handle.join() {
        Ok(s) => s,
        Err(e) => format!("Authorization thread failed {e:?}"),
    };

    let res = env.new_string(result);
    match res {
        Ok(r) => r.into_raw(),
        _ => env
            .new_string(
                serde_json::to_string(&InterfaceResult::fail_internally(
                    "Failed creating Java string".to_string(),
                ))
                .expect("could not serialise response"),
            )
            .expect("error creating Java string")
            .into_raw(),
    }
}

/// The main JNI entry point
#[jni_fn("com.cedarpolicy.BasicAuthorizationEngine")]
pub fn getCedarJNIVersion(env: JNIEnv<'_>) -> jstring {
    env.new_string("3.0")
        .expect("error creating Java string")
        .into_raw()
}

fn call_cedar(call: &str, input: &str) -> String {
    let call = String::from(call);
    let input = String::from(input);
    let result = match call.as_str() {
        V0_AUTH_OP => json_is_authorized(&input),
        V0_VALIDATE_OP => json_validate(&input),
        V0_PARSE_EUID_OP => json_parse_entity_uid(&input),
        _ => InterfaceResult::fail_internally(format!("unsupported operation: {}", call)),
    };
    serde_json::to_string(&result).expect("could not serialise response")
}

#[derive(Debug, Serialize, Deserialize)]
struct JavaInterfaceCall {
    pub call: String,
    arguments: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParseEUIDCall {
    euid: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParseEUIDOutput {
    ty: String,
    id: String,
}

fn jni_failed(env: &mut JNIEnv<'_>, e: &dyn Error) -> jvalue {
    // If we already generated an exception, then let that go up the stack
    // Otherwise, generate a cedar InternalException and return null
    if !env.exception_check().unwrap_or_default() {
        // We have to unwrap here as we're doing exception handling
        // If we don't have the heap space to create an exception, the only valid move is ending the process
        env.throw_new(
            "com/cedarpolicy/model/exception/InternalException",
            format!("Internal JNI Error: {e}"),
        )
        .unwrap();
    }
    JValueOwned::Object(JObject::null()).as_jni()
}

/// public string-based JSON interface to be invoked by FFIs. Takes in a `ParseEUIDCall`, parses it and (if successful)
/// returns a serialized `ParseEUIDOutput`
pub fn json_parse_entity_uid(input: &str) -> InterfaceResult {
    match serde_json::from_str::<ParseEUIDCall>(input) {
        Err(e) => {
            InterfaceResult::fail_internally(format!("error parsing call to parse EntityUID: {e:}"))
        }
        Ok(euid_call) => match cedar_policy::EntityUid::from_str(euid_call.euid.as_str()) {
            Ok(euid) => match serde_json::to_string(&ParseEUIDOutput {
                ty: euid.type_name().to_string(),
                id: euid.id().to_string(),
            }) {
                Ok(s) => InterfaceResult::succeed(s),
                Err(e) => {
                    InterfaceResult::fail_internally(format!("error serializing EntityUID: {e:}"))
                }
            },
            Err(e) => InterfaceResult::fail_internally(format!("error parsing EntityUID: {e:}")),
        },
    }
}

#[jni_fn("com.cedarpolicy.value.EntityIdentifier")]
pub fn getEntityIdentifierRepr<'a>(mut env: JNIEnv<'a>, _: JClass, obj: JObject<'a>) -> jvalue {
    match get_entity_identifier_repr_internal(&mut env, obj) {
        Ok(v) => v.as_jni(),
        Err(e) => jni_failed(&mut env, e.as_ref()),
    }
}

fn get_entity_identifier_repr_internal<'a>(
    env: &mut JNIEnv<'a>,
    obj: JObject<'a>,
) -> Result<JValueOwned<'a>> {
    if obj.is_null() {
        raise_npe(env)
    } else {
        let eid = JEntityId::cast(env, obj)?;
        let repr = eid.get_string_repr();
        let jstring = env.new_string(repr)?.into();
        Ok(JValueGen::Object(jstring))
    }
}

#[jni_fn("com.cedarpolicy.value.EntityTypeName")]
pub fn parseEntityTypeName<'a>(mut env: JNIEnv<'a>, _: JClass, obj: JString<'a>) -> jvalue {
    match parse_entity_type_name_internal(&mut env, obj) {
        Ok(v) => v.as_jni(),
        Err(e) => jni_failed(&mut env, e.as_ref()),
    }
}

pub fn parse_entity_type_name_internal<'a>(
    env: &mut JNIEnv<'a>,
    obj: JString<'a>,
) -> Result<JValueGen<JObject<'a>>> {
    if obj.is_null() {
        raise_npe(env)
    } else {
        let jstring = env.get_string(&obj)?;
        let src = String::from(jstring);
        JEntityTypeName::parse(env, &src).map(Into::into)
    }
}

#[jni_fn("com.cedarpolicy.value.EntityTypeName")]
pub fn getEntityTypeNameRepr<'a>(mut env: JNIEnv<'a>, _: JClass, obj: JObject<'a>) -> jvalue {
    match get_entity_type_name_repr_internal(&mut env, obj) {
        Ok(v) => v.as_jni(),
        Err(e) => jni_failed(&mut env, e.as_ref()),
    }
}

fn get_entity_type_name_repr_internal<'a>(
    env: &mut JNIEnv<'a>,
    obj: JObject<'a>,
) -> Result<JValueOwned<'a>> {
    if obj.is_null() {
        raise_npe(env)
    } else {
        let etype = JEntityTypeName::cast(env, obj)?;
        let repr = etype.get_string_repr(env)?;
        Ok(env.new_string(repr)?.into())
    }
}

#[jni_fn("com.cedarpolicy.value.EntityUID")]
pub fn parseEntityUID<'a>(mut env: JNIEnv<'a>, _: JClass, obj: JString<'a>) -> jvalue {
    let r = match parse_entity_uid_internal(&mut env, obj) {
        Ok(v) => v.as_jni(),
        Err(e) => jni_failed(&mut env, e.as_ref()),
    };
    r
}

fn parse_entity_uid_internal<'a>(
    env: &mut JNIEnv<'a>,
    obj: JString<'a>,
) -> Result<JValueOwned<'a>> {
    if obj.is_null() {
        raise_npe(env)
    } else {
        let jstring = env.get_string(&obj)?;
        let src = String::from(jstring);
        let obj = JEntityUID::parse(env, &src)?;
        Ok(obj.into())
    }
}

#[jni_fn("com.cedarpolicy.value.EntityUID")]
pub fn getEUIDRepr<'a>(
    mut env: JNIEnv<'a>,
    _: JClass,
    type_name: JObject<'a>,
    id: JObject<'a>,
) -> jvalue {
    let r = match get_euid_repr_internal(&mut env, type_name, id) {
        Ok(v) => v.as_jni(),
        Err(e) => jni_failed(&mut env, e.as_ref()),
    };
    r
}

fn get_euid_repr_internal<'a>(
    env: &mut JNIEnv<'a>,
    type_name: JObject<'a>,
    id: JObject<'a>,
) -> Result<JValueOwned<'a>> {
    if type_name.is_null() || id.is_null() {
        raise_npe(env)
    } else {
        let etype = JEntityTypeName::cast(env, type_name)?.get_rust_repr(env)?;
        let id = JEntityId::cast(env, id)?.get_rust_repr();
        let euid = EntityUid::from_type_name_and_id(etype, id);
        let jstring = env.new_string(euid.to_string())?;
        Ok(jstring.into())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_entityuid() {
        let result = call_cedar("ParseEntityUidOperation", r#"{"euid": "User::\"Alice\""} "#);
        assert_success(result);
    }

    #[test]
    fn empty_authorization_call_succeeds() {
        let result = call_cedar(
            "AuthorizationOperation",
            r#"
{
  "principal": "User::\"alice\"",
  "action": "Photo::\"view\"",
  "resource": "Photo::\"photo\"",
  "slice": {
    "policies": {},
    "entities": []
  },
  "context": {}
}
            "#,
        );
        assert_success(result);
    }

    #[test]
    fn empty_validation_call_succeeds() {
        let result = call_cedar(
            "ValidateOperation",
            r#"{ "schema": { "": {"entityTypes": {}, "actions": {} } }, "policySet": {} }"#,
        );
        assert_success(result);
    }

    #[test]
    fn unrecognised_call_fails() {
        let result = call_cedar("BadOperation", "");
        assert_failure(result);
    }

    #[test]
    fn test_unspecified_principal_call_succeeds() {
        let result = call_cedar(
            "AuthorizationOperation",
            r#"
        {
            "context": {},
            "slice": {
              "policies": {
                "001": "permit(principal, action, resource);"
              },
              "entities": [],
              "templates": {},
              "template_instantiations": []
            },
            "principal": null,
            "action": "Action::\"view\"",
            "resource": "Resource::\"thing\""
        }
        "#,
        );
        assert_success(result);
    }

    #[test]
    fn test_unspecified_resource_call_succeeds() {
        let result = call_cedar(
            "AuthorizationOperation",
            r#"
        {
            "context": {},
            "slice": {
              "policies": {
                "001": "permit(principal, action, resource);"
              },
              "entities": [],
              "templates": {},
              "template_instantiations": []
            },
            "principal": "User::\"alice\"",
            "action": "Action::\"view\"",
            "resource": null
        }
        "#,
        );
        assert_success(result);
    }

    #[test]
    fn template_authorization_call_succeeds() {
        let result = call_cedar(
            "AuthorizationOperation",
            r#"
	             { "principal": "User::\"alice\""
	             , "action" : "Photo::\"view\""
	             , "resource" : "Photo::\"door\""
	             , "context" : {}
	             , "slice" : {
	                   "policies" : {}
	                 , "entities" : []
                     , "templates" : {
                        "ID0": "permit(principal == ?principal, action, resource);"
                      }
                     , "template_instantiations" : [
                        {
                            "template_id" : "ID0",
                            "result_policy_id" : "ID0_User_alice",
                            "instantiations" : [
                                {
                                    "slot": "?principal",
                                    "value": {
                                        "ty" : "User",
                                        "eid" : "alice"
                                    }
                                }
                            ]
                        }
                     ]
	             }
	          }
	         "#,
        );
        assert_success(result);
    }

    fn assert_success(result: String) {
        let result: InterfaceResult = serde_json::from_str(result.as_str()).unwrap();
        match result {
            InterfaceResult::Success { .. } => {}
            InterfaceResult::Failure { .. } => panic!("expected a success, not {:?}", result),
        };
    }

    fn assert_failure(result: String) {
        let result: InterfaceResult = serde_json::from_str(result.as_str()).unwrap();
        match result {
            InterfaceResult::Success { .. } => panic!("expected a failure, not {:?}", result),
            InterfaceResult::Failure { .. } => {}
        };
    }
}
