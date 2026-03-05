import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { FilterOperator } from "./filteroperator.js";
export type Value = string | number | boolean;
export type FilterClause = {
    property: string;
    operator: FilterOperator;
    value: string | number | boolean;
};
/** @internal */
export declare const Value$inboundSchema: z.ZodType<Value, z.ZodTypeDef, unknown>;
/** @internal */
export type Value$Outbound = string | number | boolean;
/** @internal */
export declare const Value$outboundSchema: z.ZodType<Value$Outbound, z.ZodTypeDef, Value>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Value$ {
    /** @deprecated use `Value$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Value, z.ZodTypeDef, unknown>;
    /** @deprecated use `Value$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Value$Outbound, z.ZodTypeDef, Value>;
    /** @deprecated use `Value$Outbound` instead. */
    type Outbound = Value$Outbound;
}
export declare function valueToJSON(value: Value): string;
export declare function valueFromJSON(jsonString: string): SafeParseResult<Value, SDKValidationError>;
/** @internal */
export declare const FilterClause$inboundSchema: z.ZodType<FilterClause, z.ZodTypeDef, unknown>;
/** @internal */
export type FilterClause$Outbound = {
    property: string;
    operator: string;
    value: string | number | boolean;
};
/** @internal */
export declare const FilterClause$outboundSchema: z.ZodType<FilterClause$Outbound, z.ZodTypeDef, FilterClause>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace FilterClause$ {
    /** @deprecated use `FilterClause$inboundSchema` instead. */
    const inboundSchema: z.ZodType<FilterClause, z.ZodTypeDef, unknown>;
    /** @deprecated use `FilterClause$outboundSchema` instead. */
    const outboundSchema: z.ZodType<FilterClause$Outbound, z.ZodTypeDef, FilterClause>;
    /** @deprecated use `FilterClause$Outbound` instead. */
    type Outbound = FilterClause$Outbound;
}
export declare function filterClauseToJSON(filterClause: FilterClause): string;
export declare function filterClauseFromJSON(jsonString: string): SafeParseResult<FilterClause, SDKValidationError>;
//# sourceMappingURL=filterclause.d.ts.map