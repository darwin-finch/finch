import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomersGetExternalRequest = {
    /**
     * The customer external ID.
     */
    externalId: string;
};
/** @internal */
export declare const CustomersGetExternalRequest$inboundSchema: z.ZodType<CustomersGetExternalRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersGetExternalRequest$Outbound = {
    external_id: string;
};
/** @internal */
export declare const CustomersGetExternalRequest$outboundSchema: z.ZodType<CustomersGetExternalRequest$Outbound, z.ZodTypeDef, CustomersGetExternalRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersGetExternalRequest$ {
    /** @deprecated use `CustomersGetExternalRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersGetExternalRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersGetExternalRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersGetExternalRequest$Outbound, z.ZodTypeDef, CustomersGetExternalRequest>;
    /** @deprecated use `CustomersGetExternalRequest$Outbound` instead. */
    type Outbound = CustomersGetExternalRequest$Outbound;
}
export declare function customersGetExternalRequestToJSON(customersGetExternalRequest: CustomersGetExternalRequest): string;
export declare function customersGetExternalRequestFromJSON(jsonString: string): SafeParseResult<CustomersGetExternalRequest, SDKValidationError>;
//# sourceMappingURL=customersgetexternal.d.ts.map