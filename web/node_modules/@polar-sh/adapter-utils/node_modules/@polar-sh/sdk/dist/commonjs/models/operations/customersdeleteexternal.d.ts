import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomersDeleteExternalRequest = {
    /**
     * The customer external ID.
     */
    externalId: string;
};
/** @internal */
export declare const CustomersDeleteExternalRequest$inboundSchema: z.ZodType<CustomersDeleteExternalRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersDeleteExternalRequest$Outbound = {
    external_id: string;
};
/** @internal */
export declare const CustomersDeleteExternalRequest$outboundSchema: z.ZodType<CustomersDeleteExternalRequest$Outbound, z.ZodTypeDef, CustomersDeleteExternalRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersDeleteExternalRequest$ {
    /** @deprecated use `CustomersDeleteExternalRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersDeleteExternalRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersDeleteExternalRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersDeleteExternalRequest$Outbound, z.ZodTypeDef, CustomersDeleteExternalRequest>;
    /** @deprecated use `CustomersDeleteExternalRequest$Outbound` instead. */
    type Outbound = CustomersDeleteExternalRequest$Outbound;
}
export declare function customersDeleteExternalRequestToJSON(customersDeleteExternalRequest: CustomersDeleteExternalRequest): string;
export declare function customersDeleteExternalRequestFromJSON(jsonString: string): SafeParseResult<CustomersDeleteExternalRequest, SDKValidationError>;
//# sourceMappingURL=customersdeleteexternal.d.ts.map