import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomersDeleteRequest = {
    /**
     * The customer ID.
     */
    id: string;
};
/** @internal */
export declare const CustomersDeleteRequest$inboundSchema: z.ZodType<CustomersDeleteRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersDeleteRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomersDeleteRequest$outboundSchema: z.ZodType<CustomersDeleteRequest$Outbound, z.ZodTypeDef, CustomersDeleteRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersDeleteRequest$ {
    /** @deprecated use `CustomersDeleteRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersDeleteRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersDeleteRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersDeleteRequest$Outbound, z.ZodTypeDef, CustomersDeleteRequest>;
    /** @deprecated use `CustomersDeleteRequest$Outbound` instead. */
    type Outbound = CustomersDeleteRequest$Outbound;
}
export declare function customersDeleteRequestToJSON(customersDeleteRequest: CustomersDeleteRequest): string;
export declare function customersDeleteRequestFromJSON(jsonString: string): SafeParseResult<CustomersDeleteRequest, SDKValidationError>;
//# sourceMappingURL=customersdelete.d.ts.map