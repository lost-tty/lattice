// protobufjs/ext/descriptor — browser bundle
// Adds Root.fromDescriptor() to the global protobuf object.
// Generated from protobufjs v2.0.0 ext/descriptor + google/protobuf/descriptor.json
(function(protobuf) {
"use strict";
var exports = {};
var $protobuf = protobuf;
var _descriptorJson = {"nested":{"google":{"nested":{"protobuf":{"options":{"go_package":"google.golang.org/protobuf/types/descriptorpb","java_package":"com.google.protobuf","java_outer_classname":"DescriptorProtos","csharp_namespace":"Google.Protobuf.Reflection","objc_class_prefix":"GPB","cc_enable_arenas":true,"optimize_for":"SPEED"},"nested":{"FileDescriptorSet":{"edition":"proto2","fields":{"file":{"rule":"repeated","type":"FileDescriptorProto","id":1}},"extensions":[[536000000,536000000]]},"Edition":{"edition":"proto2","values":{"EDITION_UNKNOWN":0,"EDITION_LEGACY":900,"EDITION_PROTO2":998,"EDITION_PROTO3":999,"EDITION_2023":1000,"EDITION_2024":1001,"EDITION_1_TEST_ONLY":1,"EDITION_2_TEST_ONLY":2,"EDITION_99997_TEST_ONLY":99997,"EDITION_99998_TEST_ONLY":99998,"EDITION_99999_TEST_ONLY":99999,"EDITION_MAX":2147483647}},"FileDescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"package":{"type":"string","id":2},"dependency":{"rule":"repeated","type":"string","id":3},"publicDependency":{"rule":"repeated","type":"int32","id":10},"weakDependency":{"rule":"repeated","type":"int32","id":11},"optionDependency":{"rule":"repeated","type":"string","id":15},"messageType":{"rule":"repeated","type":"DescriptorProto","id":4},"enumType":{"rule":"repeated","type":"EnumDescriptorProto","id":5},"service":{"rule":"repeated","type":"ServiceDescriptorProto","id":6},"extension":{"rule":"repeated","type":"FieldDescriptorProto","id":7},"options":{"type":"FileOptions","id":8},"sourceCodeInfo":{"type":"SourceCodeInfo","id":9},"syntax":{"type":"string","id":12},"edition":{"type":"Edition","id":14}}},"DescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"field":{"rule":"repeated","type":"FieldDescriptorProto","id":2},"extension":{"rule":"repeated","type":"FieldDescriptorProto","id":6},"nestedType":{"rule":"repeated","type":"DescriptorProto","id":3},"enumType":{"rule":"repeated","type":"EnumDescriptorProto","id":4},"extensionRange":{"rule":"repeated","type":"ExtensionRange","id":5},"oneofDecl":{"rule":"repeated","type":"OneofDescriptorProto","id":8},"options":{"type":"MessageOptions","id":7},"reservedRange":{"rule":"repeated","type":"ReservedRange","id":9},"reservedName":{"rule":"repeated","type":"string","id":10},"visibility":{"type":"SymbolVisibility","id":11}},"nested":{"ExtensionRange":{"fields":{"start":{"type":"int32","id":1},"end":{"type":"int32","id":2},"options":{"type":"ExtensionRangeOptions","id":3}}},"ReservedRange":{"fields":{"start":{"type":"int32","id":1},"end":{"type":"int32","id":2}}}}},"ExtensionRangeOptions":{"edition":"proto2","fields":{"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999},"declaration":{"rule":"repeated","type":"Declaration","id":2,"options":{"retention":"RETENTION_SOURCE"}},"features":{"type":"FeatureSet","id":50},"verification":{"type":"VerificationState","id":3,"options":{"default":"UNVERIFIED","retention":"RETENTION_SOURCE"}}},"extensions":[[1000,536870911]],"nested":{"Declaration":{"fields":{"number":{"type":"int32","id":1},"fullName":{"type":"string","id":2},"type":{"type":"string","id":3},"reserved":{"type":"bool","id":5},"repeated":{"type":"bool","id":6}},"reserved":[[4,4]]},"VerificationState":{"values":{"DECLARATION":0,"UNVERIFIED":1}}}},"FieldDescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"number":{"type":"int32","id":3},"label":{"type":"Label","id":4},"type":{"type":"Type","id":5},"typeName":{"type":"string","id":6},"extendee":{"type":"string","id":2},"defaultValue":{"type":"string","id":7},"oneofIndex":{"type":"int32","id":9},"jsonName":{"type":"string","id":10},"options":{"type":"FieldOptions","id":8},"proto3Optional":{"type":"bool","id":17}},"nested":{"Type":{"values":{"TYPE_DOUBLE":1,"TYPE_FLOAT":2,"TYPE_INT64":3,"TYPE_UINT64":4,"TYPE_INT32":5,"TYPE_FIXED64":6,"TYPE_FIXED32":7,"TYPE_BOOL":8,"TYPE_STRING":9,"TYPE_GROUP":10,"TYPE_MESSAGE":11,"TYPE_BYTES":12,"TYPE_UINT32":13,"TYPE_ENUM":14,"TYPE_SFIXED32":15,"TYPE_SFIXED64":16,"TYPE_SINT32":17,"TYPE_SINT64":18}},"Label":{"values":{"LABEL_OPTIONAL":1,"LABEL_REPEATED":3,"LABEL_REQUIRED":2}}}},"OneofDescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"options":{"type":"OneofOptions","id":2}}},"EnumDescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"value":{"rule":"repeated","type":"EnumValueDescriptorProto","id":2},"options":{"type":"EnumOptions","id":3},"reservedRange":{"rule":"repeated","type":"EnumReservedRange","id":4},"reservedName":{"rule":"repeated","type":"string","id":5},"visibility":{"type":"SymbolVisibility","id":6}},"nested":{"EnumReservedRange":{"fields":{"start":{"type":"int32","id":1},"end":{"type":"int32","id":2}}}}},"EnumValueDescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"number":{"type":"int32","id":2},"options":{"type":"EnumValueOptions","id":3}}},"ServiceDescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"method":{"rule":"repeated","type":"MethodDescriptorProto","id":2},"options":{"type":"ServiceOptions","id":3}}},"MethodDescriptorProto":{"edition":"proto2","fields":{"name":{"type":"string","id":1},"inputType":{"type":"string","id":2},"outputType":{"type":"string","id":3},"options":{"type":"MethodOptions","id":4},"clientStreaming":{"type":"bool","id":5},"serverStreaming":{"type":"bool","id":6}}},"FileOptions":{"edition":"proto2","fields":{"javaPackage":{"type":"string","id":1},"javaOuterClassname":{"type":"string","id":8},"javaMultipleFiles":{"type":"bool","id":10},"javaGenerateEqualsAndHash":{"type":"bool","id":20,"options":{"deprecated":true}},"javaStringCheckUtf8":{"type":"bool","id":27},"optimizeFor":{"type":"OptimizeMode","id":9,"options":{"default":"SPEED"}},"goPackage":{"type":"string","id":11},"ccGenericServices":{"type":"bool","id":16},"javaGenericServices":{"type":"bool","id":17},"pyGenericServices":{"type":"bool","id":18},"deprecated":{"type":"bool","id":23},"ccEnableArenas":{"type":"bool","id":31,"options":{"default":true}},"objcClassPrefix":{"type":"string","id":36},"csharpNamespace":{"type":"string","id":37},"swiftPrefix":{"type":"string","id":39},"phpClassPrefix":{"type":"string","id":40},"phpNamespace":{"type":"string","id":41},"phpMetadataNamespace":{"type":"string","id":44},"rubyPackage":{"type":"string","id":45},"features":{"type":"FeatureSet","id":50},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]],"reserved":[[42,42],[38,38],"php_generic_services"],"nested":{"OptimizeMode":{"values":{"SPEED":1,"CODE_SIZE":2,"LITE_RUNTIME":3}}}},"MessageOptions":{"edition":"proto2","fields":{"messageSetWireFormat":{"type":"bool","id":1},"noStandardDescriptorAccessor":{"type":"bool","id":2},"deprecated":{"type":"bool","id":3},"mapEntry":{"type":"bool","id":7},"deprecatedLegacyJsonFieldConflicts":{"type":"bool","id":11,"options":{"deprecated":true}},"features":{"type":"FeatureSet","id":12},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]],"reserved":[[4,4],[5,5],[6,6],[8,8],[9,9]]},"FieldOptions":{"edition":"proto2","fields":{"ctype":{"type":"CType","id":1,"options":{"default":"STRING"}},"packed":{"type":"bool","id":2},"jstype":{"type":"JSType","id":6,"options":{"default":"JS_NORMAL"}},"lazy":{"type":"bool","id":5},"unverifiedLazy":{"type":"bool","id":15},"deprecated":{"type":"bool","id":3},"weak":{"type":"bool","id":10,"options":{"deprecated":true}},"debugRedact":{"type":"bool","id":16},"retention":{"type":"OptionRetention","id":17},"targets":{"rule":"repeated","type":"OptionTargetType","id":19},"editionDefaults":{"rule":"repeated","type":"EditionDefault","id":20},"features":{"type":"FeatureSet","id":21},"featureSupport":{"type":"FeatureSupport","id":22},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]],"reserved":[[4,4],[18,18]],"nested":{"CType":{"values":{"STRING":0,"CORD":1,"STRING_PIECE":2}},"JSType":{"values":{"JS_NORMAL":0,"JS_STRING":1,"JS_NUMBER":2}},"OptionRetention":{"values":{"RETENTION_UNKNOWN":0,"RETENTION_RUNTIME":1,"RETENTION_SOURCE":2}},"OptionTargetType":{"values":{"TARGET_TYPE_UNKNOWN":0,"TARGET_TYPE_FILE":1,"TARGET_TYPE_EXTENSION_RANGE":2,"TARGET_TYPE_MESSAGE":3,"TARGET_TYPE_FIELD":4,"TARGET_TYPE_ONEOF":5,"TARGET_TYPE_ENUM":6,"TARGET_TYPE_ENUM_ENTRY":7,"TARGET_TYPE_SERVICE":8,"TARGET_TYPE_METHOD":9}},"EditionDefault":{"fields":{"edition":{"type":"Edition","id":3},"value":{"type":"string","id":2}}},"FeatureSupport":{"fields":{"editionIntroduced":{"type":"Edition","id":1},"editionDeprecated":{"type":"Edition","id":2},"deprecationWarning":{"type":"string","id":3},"editionRemoved":{"type":"Edition","id":4}}}}},"OneofOptions":{"edition":"proto2","fields":{"features":{"type":"FeatureSet","id":1},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]]},"EnumOptions":{"edition":"proto2","fields":{"allowAlias":{"type":"bool","id":2},"deprecated":{"type":"bool","id":3},"deprecatedLegacyJsonFieldConflicts":{"type":"bool","id":6,"options":{"deprecated":true}},"features":{"type":"FeatureSet","id":7},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]],"reserved":[[5,5]]},"EnumValueOptions":{"edition":"proto2","fields":{"deprecated":{"type":"bool","id":1},"features":{"type":"FeatureSet","id":2},"debugRedact":{"type":"bool","id":3},"featureSupport":{"type":"FieldOptions.FeatureSupport","id":4},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]]},"ServiceOptions":{"edition":"proto2","fields":{"features":{"type":"FeatureSet","id":34},"deprecated":{"type":"bool","id":33},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]]},"MethodOptions":{"edition":"proto2","fields":{"deprecated":{"type":"bool","id":33},"idempotencyLevel":{"type":"IdempotencyLevel","id":34,"options":{"default":"IDEMPOTENCY_UNKNOWN"}},"features":{"type":"FeatureSet","id":35},"uninterpretedOption":{"rule":"repeated","type":"UninterpretedOption","id":999}},"extensions":[[1000,536870911]],"nested":{"IdempotencyLevel":{"values":{"IDEMPOTENCY_UNKNOWN":0,"NO_SIDE_EFFECTS":1,"IDEMPOTENT":2}}}},"UninterpretedOption":{"edition":"proto2","fields":{"name":{"rule":"repeated","type":"NamePart","id":2},"identifierValue":{"type":"string","id":3},"positiveIntValue":{"type":"uint64","id":4},"negativeIntValue":{"type":"int64","id":5},"doubleValue":{"type":"double","id":6},"stringValue":{"type":"bytes","id":7},"aggregateValue":{"type":"string","id":8}},"nested":{"NamePart":{"fields":{"namePart":{"rule":"required","type":"string","id":1},"isExtension":{"rule":"required","type":"bool","id":2}}}}},"FeatureSet":{"edition":"proto2","fields":{"fieldPresence":{"type":"FieldPresence","id":1,"options":{"retention":"RETENTION_RUNTIME","targets":"TARGET_TYPE_FILE","feature_support.edition_introduced":"EDITION_2023","edition_defaults.edition":"EDITION_2023","edition_defaults.value":"EXPLICIT"}},"enumType":{"type":"EnumType","id":2,"options":{"retention":"RETENTION_RUNTIME","targets":"TARGET_TYPE_FILE","feature_support.edition_introduced":"EDITION_2023","edition_defaults.edition":"EDITION_PROTO3","edition_defaults.value":"OPEN"}},"repeatedFieldEncoding":{"type":"RepeatedFieldEncoding","id":3,"options":{"retention":"RETENTION_RUNTIME","targets":"TARGET_TYPE_FILE","feature_support.edition_introduced":"EDITION_2023","edition_defaults.edition":"EDITION_PROTO3","edition_defaults.value":"PACKED"}},"utf8Validation":{"type":"Utf8Validation","id":4,"options":{"retention":"RETENTION_RUNTIME","targets":"TARGET_TYPE_FILE","feature_support.edition_introduced":"EDITION_2023","edition_defaults.edition":"EDITION_PROTO3","edition_defaults.value":"VERIFY"}},"messageEncoding":{"type":"MessageEncoding","id":5,"options":{"retention":"RETENTION_RUNTIME","targets":"TARGET_TYPE_FILE","feature_support.edition_introduced":"EDITION_2023","edition_defaults.edition":"EDITION_LEGACY","edition_defaults.value":"LENGTH_PREFIXED"}},"jsonFormat":{"type":"JsonFormat","id":6,"options":{"retention":"RETENTION_RUNTIME","targets":"TARGET_TYPE_FILE","feature_support.edition_introduced":"EDITION_2023","edition_defaults.edition":"EDITION_PROTO3","edition_defaults.value":"ALLOW"}},"enforceNamingStyle":{"type":"EnforceNamingStyle","id":7,"options":{"retention":"RETENTION_SOURCE","targets":"TARGET_TYPE_METHOD","feature_support.edition_introduced":"EDITION_2024","edition_defaults.edition":"EDITION_2024","edition_defaults.value":"STYLE2024"}},"defaultSymbolVisibility":{"type":"VisibilityFeature.DefaultSymbolVisibility","id":8,"options":{"retention":"RETENTION_SOURCE","targets":"TARGET_TYPE_FILE","feature_support.edition_introduced":"EDITION_2024","edition_defaults.edition":"EDITION_2024","edition_defaults.value":"EXPORT_TOP_LEVEL"}}},"extensions":[[1000,9994],[9995,9999],[10000,10000]],"reserved":[[999,999]],"nested":{"FieldPresence":{"values":{"FIELD_PRESENCE_UNKNOWN":0,"EXPLICIT":1,"IMPLICIT":2,"LEGACY_REQUIRED":3}},"EnumType":{"values":{"ENUM_TYPE_UNKNOWN":0,"OPEN":1,"CLOSED":2}},"RepeatedFieldEncoding":{"values":{"REPEATED_FIELD_ENCODING_UNKNOWN":0,"PACKED":1,"EXPANDED":2}},"Utf8Validation":{"values":{"UTF8_VALIDATION_UNKNOWN":0,"VERIFY":2,"NONE":3}},"MessageEncoding":{"values":{"MESSAGE_ENCODING_UNKNOWN":0,"LENGTH_PREFIXED":1,"DELIMITED":2}},"JsonFormat":{"values":{"JSON_FORMAT_UNKNOWN":0,"ALLOW":1,"LEGACY_BEST_EFFORT":2}},"EnforceNamingStyle":{"values":{"ENFORCE_NAMING_STYLE_UNKNOWN":0,"STYLE2024":1,"STYLE_LEGACY":2}},"VisibilityFeature":{"fields":{},"reserved":[[1,536870911]],"nested":{"DefaultSymbolVisibility":{"values":{"DEFAULT_SYMBOL_VISIBILITY_UNKNOWN":0,"EXPORT_ALL":1,"EXPORT_TOP_LEVEL":2,"LOCAL_ALL":3,"STRICT":4}}}}}},"FeatureSetDefaults":{"edition":"proto2","fields":{"defaults":{"rule":"repeated","type":"FeatureSetEditionDefault","id":1},"minimumEdition":{"type":"Edition","id":4},"maximumEdition":{"type":"Edition","id":5}},"nested":{"FeatureSetEditionDefault":{"fields":{"edition":{"type":"Edition","id":3},"overridableFeatures":{"type":"FeatureSet","id":4},"fixedFeatures":{"type":"FeatureSet","id":5}},"reserved":[[1,1],[2,2],"features"]}}},"SourceCodeInfo":{"edition":"proto2","fields":{"location":{"rule":"repeated","type":"Location","id":1}},"extensions":[[536000000,536000000]],"nested":{"Location":{"fields":{"path":{"rule":"repeated","type":"int32","id":1,"options":{"packed":true}},"span":{"rule":"repeated","type":"int32","id":2,"options":{"packed":true}},"leadingComments":{"type":"string","id":3},"trailingComments":{"type":"string","id":4},"leadingDetachedComments":{"rule":"repeated","type":"string","id":6}}}}},"GeneratedCodeInfo":{"edition":"proto2","fields":{"annotation":{"rule":"repeated","type":"Annotation","id":1}},"nested":{"Annotation":{"fields":{"path":{"rule":"repeated","type":"int32","id":1,"options":{"packed":true}},"sourceFile":{"type":"string","id":2},"begin":{"type":"int32","id":3},"end":{"type":"int32","id":4},"semantic":{"type":"Semantic","id":5}},"nested":{"Semantic":{"values":{"NONE":0,"SET":1,"ALIAS":2}}}}}},"SymbolVisibility":{"edition":"proto2","values":{"VISIBILITY_UNSET":0,"VISIBILITY_LOCAL":1,"VISIBILITY_EXPORT":2}}}}}}}};

$protobuf.descriptor = exports = $protobuf.Root.fromJSON(_descriptorJson).lookup(".google.protobuf");

var Namespace = $protobuf.Namespace,
    Root      = $protobuf.Root,
    Enum      = $protobuf.Enum,
    Type      = $protobuf.Type,
    Field     = $protobuf.Field,
    MapField  = $protobuf.MapField,
    OneOf     = $protobuf.OneOf,
    Service   = $protobuf.Service,
    Method    = $protobuf.Method;

// --- Root ---

/**
 * Properties of a FileDescriptorSet message.
 * @interface IFileDescriptorSet
 * @property {IFileDescriptorProto[]} file Files
 */

/**
 * Properties of a FileDescriptorProto message.
 * @interface IFileDescriptorProto
 * @property {string} [name] File name
 * @property {string} [package] Package
 * @property {*} [dependency] Not supported
 * @property {*} [publicDependency] Not supported
 * @property {*} [weakDependency] Not supported
 * @property {IDescriptorProto[]} [messageType] Nested message types
 * @property {IEnumDescriptorProto[]} [enumType] Nested enums
 * @property {IServiceDescriptorProto[]} [service] Nested services
 * @property {IFieldDescriptorProto[]} [extension] Nested extension fields
 * @property {IFileOptions} [options] Options
 * @property {*} [sourceCodeInfo] Not supported
 * @property {string} [syntax="proto2"] Syntax
 * @property {IEdition} [edition] Edition
 */

/**
 * Values of the Edition enum.
 * @typedef IEdition
 * @type {number}
 * @property {number} EDITION_UNKNOWN=0
 * @property {number} EDITION_LEGACY=900
 * @property {number} EDITION_PROTO2=998
 * @property {number} EDITION_PROTO3=999
 * @property {number} EDITION_2023=1000
 * @property {number} EDITION_2024=1001
 * @property {number} EDITION_1_TEST_ONLY=1
 * @property {number} EDITION_2_TEST_ONLY=2
 * @property {number} EDITION_99997_TEST_ONLY=99997
 * @property {number} EDITION_99998_TEST_ONLY=99998
 * @property {number} EDITION_99998_TEST_ONLY=99999
 * @property {number} EDITION_MAX=2147483647
 */

/**
 * Properties of a FileOptions message.
 * @interface IFileOptions
 * @property {string} [javaPackage]
 * @property {string} [javaOuterClassname]
 * @property {boolean} [javaMultipleFiles]
 * @property {boolean} [javaGenerateEqualsAndHash]
 * @property {boolean} [javaStringCheckUtf8]
 * @property {IFileOptionsOptimizeMode} [optimizeFor=1]
 * @property {string} [goPackage]
 * @property {boolean} [ccGenericServices]
 * @property {boolean} [javaGenericServices]
 * @property {boolean} [pyGenericServices]
 * @property {boolean} [deprecated]
 * @property {boolean} [ccEnableArenas]
 * @property {string} [objcClassPrefix]
 * @property {string} [csharpNamespace]
 */

/**
 * Values of he FileOptions.OptimizeMode enum.
 * @typedef IFileOptionsOptimizeMode
 * @type {number}
 * @property {number} SPEED=1
 * @property {number} CODE_SIZE=2
 * @property {number} LITE_RUNTIME=3
 */

/**
 * Creates a root from a descriptor set.
 * @param {IFileDescriptorSet|Reader|Uint8Array} descriptor Descriptor
 * @returns {Root} Root instance
 */
Root.fromDescriptor = function fromDescriptor(descriptor) {

    // Decode the descriptor message if specified as a buffer:
    if (typeof descriptor.length === "number")
        descriptor = exports.FileDescriptorSet.decode(descriptor);

    var root = new Root();

    if (descriptor.file) {
        var fileDescriptor,
            filePackage;
        for (var j = 0, i; j < descriptor.file.length; ++j) {
            filePackage = root;
            if ((fileDescriptor = descriptor.file[j])["package"] && fileDescriptor["package"].length)
                filePackage = root.define(fileDescriptor["package"]);
            var edition = editionFromDescriptor(fileDescriptor);
            if (fileDescriptor.name && fileDescriptor.name.length)
                root.files.push(filePackage.filename = fileDescriptor.name);
            if (fileDescriptor.messageType)
                for (i = 0; i < fileDescriptor.messageType.length; ++i)
                    filePackage.add(Type.fromDescriptor(fileDescriptor.messageType[i], edition));
            if (fileDescriptor.enumType)
                for (i = 0; i < fileDescriptor.enumType.length; ++i)
                    filePackage.add(Enum.fromDescriptor(fileDescriptor.enumType[i], edition));
            if (fileDescriptor.extension)
                for (i = 0; i < fileDescriptor.extension.length; ++i)
                    filePackage.add(Field.fromDescriptor(fileDescriptor.extension[i], edition));
            if (fileDescriptor.service)
                for (i = 0; i < fileDescriptor.service.length; ++i)
                    filePackage.add(Service.fromDescriptor(fileDescriptor.service[i], edition));
            var opts = fromDescriptorOptions(fileDescriptor.options, exports.FileOptions);
            if (opts) {
                var ks = Object.keys(opts);
                for (i = 0; i < ks.length; ++i)
                    filePackage.setOption(ks[i], opts[ks[i]]);
            }
        }
    }

    return root.resolveAll();
};

/**
 * Converts a root to a descriptor set.
 * @returns {Message<IFileDescriptorSet>} Descriptor
 * @param {string} [edition="proto2"] The syntax or edition to use
 */
Root.prototype.toDescriptor = function toDescriptor(edition) {
    var set = exports.FileDescriptorSet.create();
    Root_toDescriptorRecursive(this, set.file, edition);
    return set;
};

// Traverses a namespace and assembles the descriptor set
function Root_toDescriptorRecursive(ns, files, edition) {

    // Create a new file
    var file = exports.FileDescriptorProto.create({ name: ns.filename || (ns.fullName.substring(1).replace(/\./g, "_") || "root") + ".proto" });
    editionToDescriptor(edition, file);
    if (!(ns instanceof Root))
        file["package"] = ns.fullName.substring(1);

    // Add nested types
    for (var i = 0, nested; i < ns.nestedArray.length; ++i)
        if ((nested = ns._nestedArray[i]) instanceof Type)
            file.messageType.push(nested.toDescriptor(edition));
        else if (nested instanceof Enum)
            file.enumType.push(nested.toDescriptor());
        else if (nested instanceof Field)
            file.extension.push(nested.toDescriptor(edition));
        else if (nested instanceof Service)
            file.service.push(nested.toDescriptor());
        else if (nested instanceof /* plain */ Namespace)
            Root_toDescriptorRecursive(nested, files, edition); // requires new file

    // Keep package-level options
    file.options = toDescriptorOptions(ns.options, exports.FileOptions);

    // And keep the file only if there is at least one nested object
    if (file.messageType.length + file.enumType.length + file.extension.length + file.service.length)
        files.push(file);
}

// --- Type ---

/**
 * Properties of a DescriptorProto message.
 * @interface IDescriptorProto
 * @property {string} [name] Message type name
 * @property {IFieldDescriptorProto[]} [field] Fields
 * @property {IFieldDescriptorProto[]} [extension] Extension fields
 * @property {IDescriptorProto[]} [nestedType] Nested message types
 * @property {IEnumDescriptorProto[]} [enumType] Nested enums
 * @property {IDescriptorProtoExtensionRange[]} [extensionRange] Extension ranges
 * @property {IOneofDescriptorProto[]} [oneofDecl] Oneofs
 * @property {IMessageOptions} [options] Not supported
 * @property {IDescriptorProtoReservedRange[]} [reservedRange] Reserved ranges
 * @property {string[]} [reservedName] Reserved names
 */

/**
 * Properties of a MessageOptions message.
 * @interface IMessageOptions
 * @property {boolean} [mapEntry=false] Whether this message is a map entry
 */

/**
 * Properties of an ExtensionRange message.
 * @interface IDescriptorProtoExtensionRange
 * @property {number} [start] Start field id
 * @property {number} [end] End field id
 */

/**
 * Properties of a ReservedRange message.
 * @interface IDescriptorProtoReservedRange
 * @property {number} [start] Start field id
 * @property {number} [end] End field id
 */

var unnamedMessageIndex = 0;

/**
 * Creates a type from a descriptor.
 *
 * Warning: this is not safe to use with editions protos, since it discards relevant file context.
 *
 * @param {IDescriptorProto|Reader|Uint8Array} descriptor Descriptor
 * @param {string} [edition="proto2"] The syntax or edition to use
 * @param {boolean} [nested=false] Whether or not this is a nested object
 * @returns {Type} Type instance
 */
Type.fromDescriptor = function fromDescriptor(descriptor, edition, nested) {
    // Decode the descriptor message if specified as a buffer:
    if (typeof descriptor.length === "number")
        descriptor = exports.DescriptorProto.decode(descriptor);

    // Create the message type
    var type = new Type(descriptor.name.length ? descriptor.name : "Type" + unnamedMessageIndex++, fromDescriptorOptions(descriptor.options, exports.MessageOptions)),
        i;

    if (!nested)
        type._edition = edition;

    /* Oneofs */ if (descriptor.oneofDecl)
        for (i = 0; i < descriptor.oneofDecl.length; ++i)
            type.add(OneOf.fromDescriptor(descriptor.oneofDecl[i]));
    /* Fields */ if (descriptor.field)
        for (i = 0; i < descriptor.field.length; ++i) {
            var field = Field.fromDescriptor(descriptor.field[i], edition, true);
            type.add(field);
            if (descriptor.field[i].hasOwnProperty("oneofIndex")) // eslint-disable-line no-prototype-builtins
                type.oneofsArray[descriptor.field[i].oneofIndex].add(field);
        }
    /* Extension fields */ if (descriptor.extension)
        for (i = 0; i < descriptor.extension.length; ++i)
            type.add(Field.fromDescriptor(descriptor.extension[i], edition, true));
    /* Nested types */ if (descriptor.nestedType)
        for (i = 0; i < descriptor.nestedType.length; ++i) {
            type.add(Type.fromDescriptor(descriptor.nestedType[i], edition, true));
            if (descriptor.nestedType[i].options && descriptor.nestedType[i].options.mapEntry)
                type.setOption("map_entry", true);
        }
    /* Nested enums */ if (descriptor.enumType)
        for (i = 0; i < descriptor.enumType.length; ++i)
            type.add(Enum.fromDescriptor(descriptor.enumType[i], edition, true));
    /* Extension ranges */ if (descriptor.extensionRange && descriptor.extensionRange.length) {
        type.extensions = [];
        for (i = 0; i < descriptor.extensionRange.length; ++i)
            type.extensions.push([ descriptor.extensionRange[i].start, descriptor.extensionRange[i].end ]);
    }
    /* Reserved... */ if (descriptor.reservedRange && descriptor.reservedRange.length || descriptor.reservedName && descriptor.reservedName.length) {
        type.reserved = [];
        /* Ranges */ if (descriptor.reservedRange)
            for (i = 0; i < descriptor.reservedRange.length; ++i)
                type.reserved.push([ descriptor.reservedRange[i].start, descriptor.reservedRange[i].end ]);
        /* Names */ if (descriptor.reservedName)
            for (i = 0; i < descriptor.reservedName.length; ++i)
                type.reserved.push(descriptor.reservedName[i]);
    }

    return type;
};

/**
 * Converts a type to a descriptor.
 * @returns {Message<IDescriptorProto>} Descriptor
 * @param {string} [edition="proto2"] The syntax or edition to use
 */
Type.prototype.toDescriptor = function toDescriptor(edition) {
    var descriptor = exports.DescriptorProto.create({ name: this.name }),
        i;

    /* Fields */ for (i = 0; i < this.fieldsArray.length; ++i) {
        var fieldDescriptor;
        descriptor.field.push(fieldDescriptor = this._fieldsArray[i].toDescriptor(edition));
        if (this._fieldsArray[i] instanceof MapField) { // map fields are repeated FieldNameEntry
            var keyType = toDescriptorType(this._fieldsArray[i].keyType, this._fieldsArray[i].resolvedKeyType, false),
                valueType = toDescriptorType(this._fieldsArray[i].type, this._fieldsArray[i].resolvedType, false),
                valueTypeName = valueType === /* type */ 11 || valueType === /* enum */ 14
                    ? this._fieldsArray[i].resolvedType && shortname(this.parent, this._fieldsArray[i].resolvedType) || this._fieldsArray[i].type
                    : undefined;
            descriptor.nestedType.push(exports.DescriptorProto.create({
                name: fieldDescriptor.typeName,
                field: [
                    exports.FieldDescriptorProto.create({ name: "key", number: 1, label: 1, type: keyType }), // can't reference a type or enum
                    exports.FieldDescriptorProto.create({ name: "value", number: 2, label: 1, type: valueType, typeName: valueTypeName })
                ],
                options: exports.MessageOptions.create({ mapEntry: true })
            }));
        }
    }
    /* Oneofs */ for (i = 0; i < this.oneofsArray.length; ++i)
        descriptor.oneofDecl.push(this._oneofsArray[i].toDescriptor());
    /* Nested... */ for (i = 0; i < this.nestedArray.length; ++i) {
        /* Extension fields */ if (this._nestedArray[i] instanceof Field)
            descriptor.field.push(this._nestedArray[i].toDescriptor(edition));
        /* Types */ else if (this._nestedArray[i] instanceof Type)
            descriptor.nestedType.push(this._nestedArray[i].toDescriptor(edition));
        /* Enums */ else if (this._nestedArray[i] instanceof Enum)
            descriptor.enumType.push(this._nestedArray[i].toDescriptor());
        // plain nested namespaces become packages instead in Root#toDescriptor
    }
    /* Extension ranges */ if (this.extensions)
        for (i = 0; i < this.extensions.length; ++i)
            descriptor.extensionRange.push(exports.DescriptorProto.ExtensionRange.create({ start: this.extensions[i][0], end: this.extensions[i][1] }));
    /* Reserved... */ if (this.reserved)
        for (i = 0; i < this.reserved.length; ++i)
            /* Names */ if (typeof this.reserved[i] === "string")
                descriptor.reservedName.push(this.reserved[i]);
            /* Ranges */ else
                descriptor.reservedRange.push(exports.DescriptorProto.ReservedRange.create({ start: this.reserved[i][0], end: this.reserved[i][1] }));

    descriptor.options = toDescriptorOptions(this.options, exports.MessageOptions);

    return descriptor;
};

// --- Field ---

/**
 * Properties of a FieldDescriptorProto message.
 * @interface IFieldDescriptorProto
 * @property {string} [name] Field name
 * @property {number} [number] Field id
 * @property {IFieldDescriptorProtoLabel} [label] Field rule
 * @property {IFieldDescriptorProtoType} [type] Field basic type
 * @property {string} [typeName] Field type name
 * @property {string} [extendee] Extended type name
 * @property {string} [defaultValue] Literal default value
 * @property {number} [oneofIndex] Oneof index if part of a oneof
 * @property {*} [jsonName] Not supported
 * @property {IFieldOptions} [options] Field options
 */

/**
 * Values of the FieldDescriptorProto.Label enum.
 * @typedef IFieldDescriptorProtoLabel
 * @type {number}
 * @property {number} LABEL_OPTIONAL=1
 * @property {number} LABEL_REQUIRED=2
 * @property {number} LABEL_REPEATED=3
 */

/**
 * Values of the FieldDescriptorProto.Type enum.
 * @typedef IFieldDescriptorProtoType
 * @type {number}
 * @property {number} TYPE_DOUBLE=1
 * @property {number} TYPE_FLOAT=2
 * @property {number} TYPE_INT64=3
 * @property {number} TYPE_UINT64=4
 * @property {number} TYPE_INT32=5
 * @property {number} TYPE_FIXED64=6
 * @property {number} TYPE_FIXED32=7
 * @property {number} TYPE_BOOL=8
 * @property {number} TYPE_STRING=9
 * @property {number} TYPE_GROUP=10
 * @property {number} TYPE_MESSAGE=11
 * @property {number} TYPE_BYTES=12
 * @property {number} TYPE_UINT32=13
 * @property {number} TYPE_ENUM=14
 * @property {number} TYPE_SFIXED32=15
 * @property {number} TYPE_SFIXED64=16
 * @property {number} TYPE_SINT32=17
 * @property {number} TYPE_SINT64=18
 */

/**
 * Properties of a FieldOptions message.
 * @interface IFieldOptions
 * @property {boolean} [packed] Whether packed or not (defaults to `false` for proto2 and `true` for proto3)
 * @property {IFieldOptionsJSType} [jstype] JavaScript value type (not used by protobuf.js)
 */

/**
 * Values of the FieldOptions.JSType enum.
 * @typedef IFieldOptionsJSType
 * @type {number}
 * @property {number} JS_NORMAL=0
 * @property {number} JS_STRING=1
 * @property {number} JS_NUMBER=2
 */

// copied here from parse.js
var numberRe = /^(?![eE])[0-9]*(?:\.[0-9]*)?(?:[eE][+-]?[0-9]+)?$/;

/**
 * Creates a field from a descriptor.
 *
 * Warning: this is not safe to use with editions protos, since it discards relevant file context.
 *
 * @param {IFieldDescriptorProto|Reader|Uint8Array} descriptor Descriptor
 * @param {string} [edition="proto2"] The syntax or edition to use
 * @param {boolean} [nested=false] Whether or not this is a top-level object
 * @returns {Field} Field instance
 */
Field.fromDescriptor = function fromDescriptor(descriptor, edition, nested) {

    // Decode the descriptor message if specified as a buffer:
    if (typeof descriptor.length === "number")
        descriptor = exports.DescriptorProto.decode(descriptor);

    if (typeof descriptor.number !== "number")
        throw Error("missing field id");

    // Rewire field type
    var fieldType;
    if (descriptor.typeName && descriptor.typeName.length)
        fieldType = descriptor.typeName;
    else
        fieldType = fromDescriptorType(descriptor.type);

    // Rewire field rule
    var fieldRule;
    switch (descriptor.label) {
        // 0 is reserved for errors
        case 1: fieldRule = undefined; break;
        case 2: fieldRule = "required"; break;
        case 3: fieldRule = "repeated"; break;
        default: throw Error("illegal label: " + descriptor.label);
    }

	var extendee = descriptor.extendee;
	if (descriptor.extendee !== undefined) {
		extendee = extendee.length ? extendee : undefined;
	}
    var field = new Field(
        descriptor.name.length ? descriptor.name : "field" + descriptor.number,
        descriptor.number,
        fieldType,
        fieldRule,
        extendee
    );

    if (!nested)
        field._edition = edition;

    field.options = fromDescriptorOptions(descriptor.options, exports.FieldOptions);
    if (descriptor.proto3_optional)
        field.options.proto3_optional = true;

    if (descriptor.defaultValue && descriptor.defaultValue.length) {
        var defaultValue = descriptor.defaultValue;
        switch (defaultValue) {
            case "true": case "TRUE":
                defaultValue = true;
                break;
            case "false": case "FALSE":
                defaultValue = false;
                break;
            default:
                var match = numberRe.exec(defaultValue);
                if (match)
                    defaultValue = parseInt(defaultValue); // eslint-disable-line radix
                break;
        }
        field.setOption("default", defaultValue);
    }

    if (packableDescriptorType(descriptor.type)) {
        if (edition === "proto3") { // defaults to packed=true (internal preset is packed=true)
            if (descriptor.options && !descriptor.options.packed)
                field.setOption("packed", false);
        } else if ((!edition || edition === "proto2") && descriptor.options && descriptor.options.packed) // defaults to packed=false
            field.setOption("packed", true);
    }

    return field;
};

/**
 * Converts a field to a descriptor.
 * @returns {Message<IFieldDescriptorProto>} Descriptor
 * @param {string} [edition="proto2"] The syntax or edition to use
 */
Field.prototype.toDescriptor = function toDescriptor(edition) {
    var descriptor = exports.FieldDescriptorProto.create({ name: this.name, number: this.id });

    if (this.map) {

        descriptor.type = 11; // message
        descriptor.typeName = $protobuf.util.ucFirst(this.name); // fieldName -> FieldNameEntry (built in Type#toDescriptor)
        descriptor.label = 3; // repeated

    } else {

        // Rewire field type
        switch (descriptor.type = toDescriptorType(this.type, this.resolve().resolvedType, this.delimited)) {
            case 10: // group
            case 11: // type
            case 14: // enum
                descriptor.typeName = this.resolvedType ? shortname(this.parent, this.resolvedType) : this.type;
                break;
        }

        // Rewire field rule
        if (this.rule === "repeated") {
            descriptor.label = 3;
        } else if (this.required && edition === "proto2") {
            descriptor.label = 2;
        } else {
            descriptor.label = 1;
        }
    }

    // Handle extension field
    descriptor.extendee = this.extensionField ? this.extensionField.parent.fullName : this.extend;

    // Handle part of oneof
    if (this.partOf)
        if ((descriptor.oneofIndex = this.parent.oneofsArray.indexOf(this.partOf)) < 0)
            throw Error("missing oneof");

    if (this.options) {
        descriptor.options = toDescriptorOptions(this.options, exports.FieldOptions);
        if (this.options["default"] != null)
            descriptor.defaultValue = String(this.options["default"]);
        if (this.options.proto3_optional)
            descriptor.proto3_optional = true;
    }

    if (edition === "proto3") { // defaults to packed=true
        if (!this.packed)
            (descriptor.options || (descriptor.options = exports.FieldOptions.create())).packed = false;
    } else if ((!edition || edition === "proto2") && this.packed) // defaults to packed=false
        (descriptor.options || (descriptor.options = exports.FieldOptions.create())).packed = true;

    return descriptor;
};

// --- Enum ---

/**
 * Properties of an EnumDescriptorProto message.
 * @interface IEnumDescriptorProto
 * @property {string} [name] Enum name
 * @property {IEnumValueDescriptorProto[]} [value] Enum values
 * @property {IEnumOptions} [options] Enum options
 */

/**
 * Properties of an EnumValueDescriptorProto message.
 * @interface IEnumValueDescriptorProto
 * @property {string} [name] Name
 * @property {number} [number] Value
 * @property {*} [options] Not supported
 */

/**
 * Properties of an EnumOptions message.
 * @interface IEnumOptions
 * @property {boolean} [allowAlias] Whether aliases are allowed
 * @property {boolean} [deprecated]
 */

var unnamedEnumIndex = 0;

/**
 * Creates an enum from a descriptor.
 *
 * Warning: this is not safe to use with editions protos, since it discards relevant file context.
 *
 * @param {IEnumDescriptorProto|Reader|Uint8Array} descriptor Descriptor
 * @param {string} [edition="proto2"] The syntax or edition to use
 * @param {boolean} [nested=false] Whether or not this is a top-level object
 * @returns {Enum} Enum instance
 */
Enum.fromDescriptor = function fromDescriptor(descriptor, edition, nested) {

    // Decode the descriptor message if specified as a buffer:
    if (typeof descriptor.length === "number")
        descriptor = exports.EnumDescriptorProto.decode(descriptor);

    // Construct values object
    var values = {};
    if (descriptor.value)
        for (var i = 0; i < descriptor.value.length; ++i) {
            var name  = descriptor.value[i].name,
                value = descriptor.value[i].number || 0;
            values[name && name.length ? name : "NAME" + value] = value;
        }

    var enm = new Enum(
        descriptor.name && descriptor.name.length ? descriptor.name : "Enum" + unnamedEnumIndex++,
        values,
        fromDescriptorOptions(descriptor.options, exports.EnumOptions)
    );

    if (!nested)
        enm._edition = edition;

    return enm;
};

/**
 * Converts an enum to a descriptor.
 * @returns {Message<IEnumDescriptorProto>} Descriptor
 */
Enum.prototype.toDescriptor = function toDescriptor() {

    // Values
    var values = [];
    for (var i = 0, ks = Object.keys(this.values); i < ks.length; ++i)
        values.push(exports.EnumValueDescriptorProto.create({ name: ks[i], number: this.values[ks[i]] }));

    return exports.EnumDescriptorProto.create({
        name: this.name,
        value: values,
        options: toDescriptorOptions(this.options, exports.EnumOptions)
    });
};

// --- OneOf ---

/**
 * Properties of a OneofDescriptorProto message.
 * @interface IOneofDescriptorProto
 * @property {string} [name] Oneof name
 * @property {*} [options] Not supported
 */

var unnamedOneofIndex = 0;

/**
 * Creates a oneof from a descriptor.
 *
 * Warning: this is not safe to use with editions protos, since it discards relevant file context.
 *
 * @param {IOneofDescriptorProto|Reader|Uint8Array} descriptor Descriptor
 * @returns {OneOf} OneOf instance
 */
OneOf.fromDescriptor = function fromDescriptor(descriptor) {

    // Decode the descriptor message if specified as a buffer:
    if (typeof descriptor.length === "number")
        descriptor = exports.OneofDescriptorProto.decode(descriptor);

    return new OneOf(
        // unnamedOneOfIndex is global, not per type, because we have no ref to a type here
        descriptor.name && descriptor.name.length ? descriptor.name : "oneof" + unnamedOneofIndex++
        // fromDescriptorOptions(descriptor.options, exports.OneofOptions) - only uninterpreted_option
    );
};

/**
 * Converts a oneof to a descriptor.
 * @returns {Message<IOneofDescriptorProto>} Descriptor
 */
OneOf.prototype.toDescriptor = function toDescriptor() {
    return exports.OneofDescriptorProto.create({
        name: this.name
        // options: toDescriptorOptions(this.options, exports.OneofOptions) - only uninterpreted_option
    });
};

// --- Service ---

/**
 * Properties of a ServiceDescriptorProto message.
 * @interface IServiceDescriptorProto
 * @property {string} [name] Service name
 * @property {IMethodDescriptorProto[]} [method] Methods
 * @property {IServiceOptions} [options] Options
 */

/**
 * Properties of a ServiceOptions message.
 * @interface IServiceOptions
 * @property {boolean} [deprecated]
 */

var unnamedServiceIndex = 0;

/**
 * Creates a service from a descriptor.
 *
 * Warning: this is not safe to use with editions protos, since it discards relevant file context.
 *
 * @param {IServiceDescriptorProto|Reader|Uint8Array} descriptor Descriptor
 * @param {string} [edition="proto2"] The syntax or edition to use
 * @param {boolean} [nested=false] Whether or not this is a top-level object
 * @returns {Service} Service instance
 */
Service.fromDescriptor = function fromDescriptor(descriptor, edition, nested) {

    // Decode the descriptor message if specified as a buffer:
    if (typeof descriptor.length === "number")
        descriptor = exports.ServiceDescriptorProto.decode(descriptor);

    var service = new Service(descriptor.name && descriptor.name.length ? descriptor.name : "Service" + unnamedServiceIndex++, fromDescriptorOptions(descriptor.options, exports.ServiceOptions));
    if (!nested)
        service._edition = edition;
    if (descriptor.method)
        for (var i = 0; i < descriptor.method.length; ++i)
            service.add(Method.fromDescriptor(descriptor.method[i]));

    return service;
};

/**
 * Converts a service to a descriptor.
 * @returns {Message<IServiceDescriptorProto>} Descriptor
 */
Service.prototype.toDescriptor = function toDescriptor() {

    // Methods
    var methods = [];
    for (var i = 0; i < this.methodsArray.length; ++i)
        methods.push(this._methodsArray[i].toDescriptor());

    return exports.ServiceDescriptorProto.create({
        name: this.name,
        method: methods,
        options: toDescriptorOptions(this.options, exports.ServiceOptions)
    });
};

// --- Method ---

/**
 * Properties of a MethodDescriptorProto message.
 * @interface IMethodDescriptorProto
 * @property {string} [name] Method name
 * @property {string} [inputType] Request type name
 * @property {string} [outputType] Response type name
 * @property {IMethodOptions} [options] Not supported
 * @property {boolean} [clientStreaming=false] Whether requests are streamed
 * @property {boolean} [serverStreaming=false] Whether responses are streamed
 */

/**
 * Properties of a MethodOptions message.
 *
 * Warning: this is not safe to use with editions protos, since it discards relevant file context.
 *
 * @interface IMethodOptions
 * @property {boolean} [deprecated]
 */

var unnamedMethodIndex = 0;

/**
 * Creates a method from a descriptor.
 * @param {IMethodDescriptorProto|Reader|Uint8Array} descriptor Descriptor
 * @returns {Method} Reflected method instance
 */
Method.fromDescriptor = function fromDescriptor(descriptor) {

    // Decode the descriptor message if specified as a buffer:
    if (typeof descriptor.length === "number")
        descriptor = exports.MethodDescriptorProto.decode(descriptor);

    return new Method(
        // unnamedMethodIndex is global, not per service, because we have no ref to a service here
        descriptor.name && descriptor.name.length ? descriptor.name : "Method" + unnamedMethodIndex++,
        "rpc",
        descriptor.inputType,
        descriptor.outputType,
        Boolean(descriptor.clientStreaming),
        Boolean(descriptor.serverStreaming),
        fromDescriptorOptions(descriptor.options, exports.MethodOptions)
    );
};

/**
 * Converts a method to a descriptor.
 * @returns {Message<IMethodDescriptorProto>} Descriptor
 */
Method.prototype.toDescriptor = function toDescriptor() {
    return exports.MethodDescriptorProto.create({
        name: this.name,
        inputType: this.resolvedRequestType ? this.resolvedRequestType.fullName : this.requestType,
        outputType: this.resolvedResponseType ? this.resolvedResponseType.fullName : this.responseType,
        clientStreaming: this.requestStream,
        serverStreaming: this.responseStream,
        options: toDescriptorOptions(this.options, exports.MethodOptions)
    });
};

// --- utility ---

// Converts a descriptor type to a protobuf.js basic type
function fromDescriptorType(type) {
    switch (type) {
        // 0 is reserved for errors
        case 1: return "double";
        case 2: return "float";
        case 3: return "int64";
        case 4: return "uint64";
        case 5: return "int32";
        case 6: return "fixed64";
        case 7: return "fixed32";
        case 8: return "bool";
        case 9: return "string";
        case 12: return "bytes";
        case 13: return "uint32";
        case 15: return "sfixed32";
        case 16: return "sfixed64";
        case 17: return "sint32";
        case 18: return "sint64";
    }
    throw Error("illegal type: " + type);
}

// Tests if a descriptor type is packable
function packableDescriptorType(type) {
    switch (type) {
        case 1: // double
        case 2: // float
        case 3: // int64
        case 4: // uint64
        case 5: // int32
        case 6: // fixed64
        case 7: // fixed32
        case 8: // bool
        case 13: // uint32
        case 14: // enum (!)
        case 15: // sfixed32
        case 16: // sfixed64
        case 17: // sint32
        case 18: // sint64
            return true;
    }
    return false;
}

// Converts a protobuf.js basic type to a descriptor type
function toDescriptorType(type, resolvedType, delimited) {
    switch (type) {
        // 0 is reserved for errors
        case "double": return 1;
        case "float": return 2;
        case "int64": return 3;
        case "uint64": return 4;
        case "int32": return 5;
        case "fixed64": return 6;
        case "fixed32": return 7;
        case "bool": return 8;
        case "string": return 9;
        case "bytes": return 12;
        case "uint32": return 13;
        case "sfixed32": return 15;
        case "sfixed64": return 16;
        case "sint32": return 17;
        case "sint64": return 18;
    }
    if (resolvedType instanceof Enum)
        return 14;
    if (resolvedType instanceof Type)
        return delimited ? 10 : 11;
    throw Error("illegal type: " + type);
}

function fromDescriptorOptionsRecursive(obj, type) {
    var val = {};
    for (var i = 0, field, key; i < type.fieldsArray.length; ++i) {
        if ((key = (field = type._fieldsArray[i]).name) === "uninterpretedOption") continue;
        if (!Object.prototype.hasOwnProperty.call(obj, key)) continue;

        var newKey = underScore(key);
        if (field.resolvedType instanceof Type) {
            val[newKey] = fromDescriptorOptionsRecursive(obj[key], field.resolvedType);
        } else if(field.resolvedType instanceof Enum) {
            val[newKey] = field.resolvedType.valuesById[obj[key]];
        } else {
            val[newKey] = obj[key];
        }
    }
    return val;
}

// Converts descriptor options to an options object
function fromDescriptorOptions(options, type) {
    if (!options)
        return undefined;
    return fromDescriptorOptionsRecursive(type.toObject(options), type);
}

function toDescriptorOptionsRecursive(obj, type) {
    var val = {};
    var keys = Object.keys(obj);
    for (var i = 0; i < keys.length; ++i) {
        var key = keys[i];
        var newKey = $protobuf.util.camelCase(key);
        if (!Object.prototype.hasOwnProperty.call(type.fields, newKey)) continue;
        var field = type.fields[newKey];
        if (field.resolvedType instanceof Type) {
            val[newKey] = toDescriptorOptionsRecursive(obj[key], field.resolvedType);
        } else {
            val[newKey] = obj[key];
        }
        if (field.repeated && !Array.isArray(val[newKey])) {
            val[newKey] = [val[newKey]];
        }
    }
    return val;
}

// Converts an options object to descriptor options
function toDescriptorOptions(options, type) {
    if (!options)
        return undefined;
    return type.fromObject(toDescriptorOptionsRecursive(options, type));
}

// Calculates the shortest relative path from `from` to `to`.
function shortname(from, to) {
    var fromPath = from.fullName.split("."),
        toPath = to.fullName.split("."),
        i = 0,
        j = 0,
        k = toPath.length - 1;
    if (!(from instanceof Root) && to instanceof Namespace)
        while (i < fromPath.length && j < k && fromPath[i] === toPath[j]) {
            var other = to.lookup(fromPath[i++], true);
            if (other !== null && other !== to)
                break;
            ++j;
        }
    else
        for (; i < fromPath.length && j < k && fromPath[i] === toPath[j]; ++i, ++j);
    return toPath.slice(j).join(".");
}

// copied here from cli/targets/proto.js
function underScore(str) {
    return str.substring(0,1)
         + str.substring(1)
               .replace(/([A-Z])(?=[a-z]|$)/g, function($0, $1) { return "_" + $1.toLowerCase(); });
}

function editionFromDescriptor(fileDescriptor) {
    if (fileDescriptor.syntax === "editions") {
        switch(fileDescriptor.edition) {
            case exports.Edition.EDITION_2023:
                return "2023";
            default:
                throw new Error("Unsupported edition " + fileDescriptor.edition);
        }
    }
    if (fileDescriptor.syntax === "proto3") {
        return "proto3";
    }
    return "proto2";
}

function editionToDescriptor(edition, fileDescriptor) {
    if (!edition) return;
    if (edition === "proto2" || edition === "proto3") {
        fileDescriptor.syntax = edition;
    } else {
        fileDescriptor.syntax = "editions";
        switch(edition) {
            case "2023":
                fileDescriptor.edition = exports.Edition.EDITION_2023;
                break;
            default:
                throw new Error("Unsupported edition " + edition);
        }
    }
}

// --- exports ---

/**
 * Reflected file descriptor set.
 * @name FileDescriptorSet
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected file descriptor proto.
 * @name FileDescriptorProto
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected descriptor proto.
 * @name DescriptorProto
 * @type {Type}
 * @property {Type} ExtensionRange
 * @property {Type} ReservedRange
 * @const
 * @tstype $protobuf.Type & {
 *     ExtensionRange: $protobuf.Type,
 *     ReservedRange: $protobuf.Type
 * }
 */

/**
 * Reflected field descriptor proto.
 * @name FieldDescriptorProto
 * @type {Type}
 * @property {Enum} Label
 * @property {Enum} Type
 * @const
 * @tstype $protobuf.Type & {
 *     Label: $protobuf.Enum,
 *     Type: $protobuf.Enum
 * }
 */

/**
 * Reflected oneof descriptor proto.
 * @name OneofDescriptorProto
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected enum descriptor proto.
 * @name EnumDescriptorProto
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected service descriptor proto.
 * @name ServiceDescriptorProto
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected enum value descriptor proto.
 * @name EnumValueDescriptorProto
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected method descriptor proto.
 * @name MethodDescriptorProto
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected file options.
 * @name FileOptions
 * @type {Type}
 * @property {Enum} OptimizeMode
 * @const
 * @tstype $protobuf.Type & {
 *     OptimizeMode: $protobuf.Enum
 * }
 */

/**
 * Reflected message options.
 * @name MessageOptions
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected field options.
 * @name FieldOptions
 * @type {Type}
 * @property {Enum} CType
 * @property {Enum} JSType
 * @const
 * @tstype $protobuf.Type & {
 *     CType: $protobuf.Enum,
 *     JSType: $protobuf.Enum
 * }
 */

/**
 * Reflected oneof options.
 * @name OneofOptions
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected enum options.
 * @name EnumOptions
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected enum value options.
 * @name EnumValueOptions
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected service options.
 * @name ServiceOptions
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected method options.
 * @name MethodOptions
 * @type {Type}
 * @const
 * @tstype $protobuf.Type
 */

/**
 * Reflected uninterpretet option.
 * @name UninterpretedOption
 * @type {Type}
 * @property {Type} NamePart
 * @const
 * @tstype $protobuf.Type & {
 *     NamePart: $protobuf.Type
 * }
 */

/**
 * Reflected source code info.
 * @name SourceCodeInfo
 * @type {Type}
 * @property {Type} Location
 * @const
 * @tstype $protobuf.Type & {
 *     Location: $protobuf.Type
 * }
 */

/**
 * Reflected generated code info.
 * @name GeneratedCodeInfo
 * @type {Type}
 * @property {Type} Annotation
 * @const
 * @tstype $protobuf.Type & {
 *     Annotation: $protobuf.Type
 * }
 */
})(protobuf);
