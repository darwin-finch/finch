import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { FilterClause, FilterClause$Outbound } from "./filterclause.js";
import { FilterConjunction } from "./filterconjunction.js";
export type Clauses = Filter | FilterClause;
export type Filter = {
    conjunction: FilterConjunction;
    clauses: Array<Filter | FilterClause>;
};
/** @internal */
export declare const Clauses$inboundSchema: z.ZodType<Clauses, z.ZodTypeDef, unknown>;
/** @internal */
export type Clauses$Outbound = Filter$Outbound | FilterClause$Outbound;
/** @internal */
export declare const Clauses$outboundSchema: z.ZodType<Clauses$Outbound, z.ZodTypeDef, Clauses>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Clauses$ {
    /** @deprecated use `Clauses$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Clauses, z.ZodTypeDef, unknown>;
    /** @deprecated use `Clauses$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Clauses$Outbound, z.ZodTypeDef, Clauses>;
    /** @deprecated use `Clauses$Outbound` instead. */
    type Outbound = Clauses$Outbound;
}
export declare function clausesToJSON(clauses: Clauses): string;
export declare function clausesFromJSON(jsonString: string): SafeParseResult<Clauses, SDKValidationError>;
/** @internal */
export declare const Filter$inboundSchema: z.ZodType<Filter, z.ZodTypeDef, unknown>;
/** @internal */
export type Filter$Outbound = {
    conjunction: string;
    clauses: Array<Filter$Outbound | FilterClause$Outbound>;
};
/** @internal */
export declare const Filter$outboundSchema: z.ZodType<Filter$Outbound, z.ZodTypeDef, Filter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Filter$ {
    /** @deprecated use `Filter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Filter, z.ZodTypeDef, unknown>;
    /** @deprecated use `Filter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Filter$Outbound, z.ZodTypeDef, Filter>;
    /** @deprecated use `Filter$Outbound` instead. */
    type Outbound = Filter$Outbound;
}
export declare function filterToJSON(filter: Filter): string;
export declare function filterFromJSON(jsonString: string): SafeParseResult<Filter, SDKValidationError>;
//# sourceMappingURL=filter.d.ts.map