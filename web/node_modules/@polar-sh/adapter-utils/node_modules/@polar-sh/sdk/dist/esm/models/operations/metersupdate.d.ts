import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { MeterUpdate, MeterUpdate$Outbound } from "../components/meterupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type MetersUpdateRequest = {
    /**
     * The meter ID.
     */
    id: string;
    meterUpdate: MeterUpdate;
};
/** @internal */
export declare const MetersUpdateRequest$inboundSchema: z.ZodType<MetersUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersUpdateRequest$Outbound = {
    id: string;
    MeterUpdate: MeterUpdate$Outbound;
};
/** @internal */
export declare const MetersUpdateRequest$outboundSchema: z.ZodType<MetersUpdateRequest$Outbound, z.ZodTypeDef, MetersUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersUpdateRequest$ {
    /** @deprecated use `MetersUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersUpdateRequest$Outbound, z.ZodTypeDef, MetersUpdateRequest>;
    /** @deprecated use `MetersUpdateRequest$Outbound` instead. */
    type Outbound = MetersUpdateRequest$Outbound;
}
export declare function metersUpdateRequestToJSON(metersUpdateRequest: MetersUpdateRequest): string;
export declare function metersUpdateRequestFromJSON(jsonString: string): SafeParseResult<MetersUpdateRequest, SDKValidationError>;
//# sourceMappingURL=metersupdate.d.ts.map