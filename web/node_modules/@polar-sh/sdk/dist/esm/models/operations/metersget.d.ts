import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type MetersGetRequest = {
    /**
     * The meter ID.
     */
    id: string;
};
/** @internal */
export declare const MetersGetRequest$inboundSchema: z.ZodType<MetersGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const MetersGetRequest$outboundSchema: z.ZodType<MetersGetRequest$Outbound, z.ZodTypeDef, MetersGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersGetRequest$ {
    /** @deprecated use `MetersGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersGetRequest$Outbound, z.ZodTypeDef, MetersGetRequest>;
    /** @deprecated use `MetersGetRequest$Outbound` instead. */
    type Outbound = MetersGetRequest$Outbound;
}
export declare function metersGetRequestToJSON(metersGetRequest: MetersGetRequest): string;
export declare function metersGetRequestFromJSON(jsonString: string): SafeParseResult<MetersGetRequest, SDKValidationError>;
//# sourceMappingURL=metersget.d.ts.map