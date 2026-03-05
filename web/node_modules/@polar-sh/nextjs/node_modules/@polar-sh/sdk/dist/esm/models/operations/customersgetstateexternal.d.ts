import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomersGetStateExternalRequest = {
    /**
     * The customer external ID.
     */
    externalId: string;
};
/** @internal */
export declare const CustomersGetStateExternalRequest$inboundSchema: z.ZodType<CustomersGetStateExternalRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersGetStateExternalRequest$Outbound = {
    external_id: string;
};
/** @internal */
export declare const CustomersGetStateExternalRequest$outboundSchema: z.ZodType<CustomersGetStateExternalRequest$Outbound, z.ZodTypeDef, CustomersGetStateExternalRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersGetStateExternalRequest$ {
    /** @deprecated use `CustomersGetStateExternalRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersGetStateExternalRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersGetStateExternalRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersGetStateExternalRequest$Outbound, z.ZodTypeDef, CustomersGetStateExternalRequest>;
    /** @deprecated use `CustomersGetStateExternalRequest$Outbound` instead. */
    type Outbound = CustomersGetStateExternalRequest$Outbound;
}
export declare function customersGetStateExternalRequestToJSON(customersGetStateExternalRequest: CustomersGetStateExternalRequest): string;
export declare function customersGetStateExternalRequestFromJSON(jsonString: string): SafeParseResult<CustomersGetStateExternalRequest, SDKValidationError>;
//# sourceMappingURL=customersgetstateexternal.d.ts.map